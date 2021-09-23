// Copyright © 2021 Translucence Research, Inc. All rights reserved.

use crate::WebState;
use futures::prelude::*;
use itertools::izip;
use phaselock::BlockContents;
use std::collections::HashMap;
use std::fmt::Debug;
use std::str::FromStr;
use strum::{AsStaticRef, IntoEnumIterator};
use strum_macros::{AsRefStr, EnumIter, EnumString};
use tagged_base64::TaggedBase64;
use tide::http::mime;
use tide::prelude::*;
use tide::sse;
use tide::StatusCode;
use tracing::{event, Level};
use zerok_lib::api::*;
use zerok_lib::node::{LedgerSummary, LedgerTransition, QueryService};

#[derive(Debug, EnumString)]
pub enum UrlSegmentType {
    Boolean,
    Hexadecimal,
    Integer,
    TaggedBase64,
    Literal,
}

#[allow(dead_code)]
#[derive(Debug)]
pub enum UrlSegmentValue {
    Boolean(bool),
    Hexadecimal(u128),
    Integer(u128),
    Identifier(TaggedBase64),
    Unparsed(String),
    ParseFailed(UrlSegmentType, String),
    Literal(String),
}

use UrlSegmentValue::*;

impl UrlSegmentValue {
    pub fn parse(value: &str, ptype: &str) -> Option<Self> {
        Some(match ptype {
            "Boolean" => Boolean(value.parse::<bool>().ok()?),
            "Hexadecimal" => Hexadecimal(u128::from_str_radix(value, 16).ok()?),
            "Integer" => Integer(value.parse::<u128>().ok()?),
            "TaggedBase64" => Identifier(TaggedBase64::parse(value).ok()?),
            _ => panic!("Type specified in api.toml isn't supported: {}", ptype),
        })
    }

    pub fn as_boolean(&self) -> Result<bool, tide::Error> {
        if let Boolean(b) = self {
            Ok(*b)
        } else {
            Err(tide::Error::from_str(
                StatusCode::BadRequest,
                format!("expected boolean, got {:?}", self),
            ))
        }
    }

    pub fn as_index(&self) -> Result<usize, tide::Error> {
        if let Integer(ix) = self {
            Ok(*ix as usize)
        } else {
            Err(tide::Error::from_str(
                StatusCode::BadRequest,
                format!("expected index, got {:?}", self),
            ))
        }
    }

    pub fn as_identifier(&self) -> Result<TaggedBase64, tide::Error> {
        if let Identifier(i) = self {
            Ok(i.clone())
        } else {
            Err(tide::Error::from_str(
                StatusCode::BadRequest,
                format!("expected tagged base 64, got {:?}", self),
            ))
        }
    }

    pub fn to<T: TaggedBlob>(&self) -> Result<T, tide::Error> {
        T::from_tagged_blob(&self.as_identifier()?)
            .map_err(|err| tide::Error::from_str(StatusCode::BadRequest, format!("{}", err)))
    }
}

fn server_error(err: impl std::error::Error + Debug + Send + Sync + 'static) -> tide::Error {
    event!(
        Level::ERROR,
        "internal error while processing request: {:?}",
        err
    );
    tide::Error::new(StatusCode::InternalServerError, err)
}

#[derive(Debug)]
pub struct RouteBinding {
    /// Placeholder from the route pattern, e.g. :id
    pub parameter: String,

    /// Type for parsing
    pub ptype: UrlSegmentType,

    /// Value
    pub value: UrlSegmentValue,
}

/// Index entries for documentation fragments
#[allow(non_camel_case_types)]
#[derive(AsRefStr, Copy, Clone, Debug, EnumIter, EnumString)]
pub enum ApiRouteKey {
    getblock,
    getblockcount,
    getblockhash,
    getblockid,
    getinfo,
    getmempool,
    gettransaction,
    getunspentrecord,
    getunspentrecordsetinfo,
    subscribe,
}

/// Verifiy that every variant of enum ApiRouteKey is defined in api.toml
// TODO !corbett Check all the other things that might fail after startup.
pub fn check_api(api: toml::Value) -> bool {
    let mut missing_definition = false;
    for key in ApiRouteKey::iter() {
        let key_str = key.as_ref();
        if api["route"].get(key_str).is_none() {
            println!("Missing API definition for [route.{}]", key_str);
            missing_definition = true;
        }
    }
    if missing_definition {
        panic!("api.toml is inconsistent with enum ApiRoutKey");
    }
    !missing_definition
}

// Get a block index from whatever form of block identifier was used in the URL.
async fn block_index(
    bindings: &HashMap<String, RouteBinding>,
    query_service: &(impl QueryService + Sync),
) -> Result<usize, tide::Error> {
    if let Some(b) = bindings.get(":index") {
        b.value.as_index()
    } else if let Some(b) = bindings.get(":bkid") {
        Ok(b.value.to::<BlockId>()?.0)
    } else if let Some(hash) = bindings.get(":hash") {
        query_service
            .get_block_id_by_hash(&hash.value.to::<Hash>()?.0)
            .await
            .map_err(server_error)
    } else {
        // latest
        Ok(query_service.num_blocks().await.map_err(server_error)? - 1)
    }
}

pub fn dummy_url_eval(
    route_pattern: &str,
    bindings: &HashMap<String, RouteBinding>,
) -> Result<tide::Response, tide::Error> {
    Ok(tide::Response::builder(200)
        .body(tide::Body::from_string(format!(
            "<!DOCTYPE html>
<html lang='en'>
  <head>
    <meta charset='utf-8'>
    <title>{}</title>
    <link rel='stylesheet' href='style.css'>
    <script src='script.js'></script>
  </head>
  <body>
    <h1>{}</h1>
    <p>{:?}</p>
  </body>
</html>",
            route_pattern.split_once('/').unwrap().0,
            route_pattern.to_string(),
            bindings
        )))
        .content_type(tide::http::mime::HTML)
        .build())
}

////////////////////////////////////////////////////////////////////////////////
// Endpoints
//
// Each endpoint function handles one API endpoint, returning an instance of
// Serialize (or an error). The main entrypoint, dispatch_url, is in charge of
// serializing the endpoint responses according to the requested content type
// and building a Response object.
//

async fn get_info(
    query_service: &(impl QueryService + Sync),
) -> Result<LedgerSummary, tide::Error> {
    query_service.get_summary().await.map_err(server_error)
}

async fn get_block_count(query_service: &(impl QueryService + Sync)) -> Result<usize, tide::Error> {
    let info = query_service.get_summary().await.map_err(server_error)?;
    Ok(info.num_blocks)
}

async fn get_block(
    bindings: &HashMap<String, RouteBinding>,
    query_service: &(impl QueryService + Sync),
) -> Result<CommittedBlock, tide::Error> {
    let index = block_index(bindings, query_service).await?;

    // Get the block and the validator state that resulted from applying this block.
    let transition = query_service.get_block(index).await.map_err(server_error)?;
    // The block numbered `index` is the block which was applied to the `index` state. Therefore,
    // the next state (the one that resulted from this block) is `index + 1`.
    let state = query_service
        .get_snapshot(index + 1)
        .await
        .map_err(server_error)?
        .state;

    Ok(CommittedBlock {
        id: BlockId(index),
        index,
        hash: Hash::from(transition.block.hash()),
        state_commitment: Hash::from(state.commit()),
        transactions: izip!(
            transition.block.block.0,
            transition.block.proofs,
            transition.memos,
            transition.uids
        )
        .enumerate()
        .map(|(i, (tx, proofs, signed_memos, uids))| {
            let (memos, sig) = match signed_memos {
                Some((memos, sig)) => (Some(memos), Some(sig)),
                None => (None, None),
            };
            CommittedTransaction {
                id: TransactionId(BlockId(index), i),
                data: tx,
                proofs,
                output_uids: uids,
                output_memos: memos,
                memos_signature: sig,
            }
        })
        .collect(),
    })
}

async fn get_block_id(
    bindings: &HashMap<String, RouteBinding>,
    query_service: &(impl QueryService + Sync),
) -> Result<BlockId, tide::Error> {
    let index = block_index(bindings, query_service).await?;
    Ok(BlockId(index))
}

async fn get_block_hash(
    bindings: &HashMap<String, RouteBinding>,
    query_service: &(impl QueryService + Sync),
) -> Result<Hash, tide::Error> {
    let index = block_index(bindings, query_service).await?;
    let block = query_service
        .get_block(index)
        .await
        .map_err(server_error)?
        .block;
    Ok(Hash::from(block.hash()))
}

async fn get_transaction(
    bindings: &HashMap<String, RouteBinding>,
    query_service: &impl QueryService,
) -> Result<CommittedTransaction, tide::Error> {
    let TransactionId(block_id, tx_id) = bindings[":txid"].value.to()?;

    // First get the block containing the transaction.
    let LedgerTransition {
        mut block,
        mut memos,
        mut uids,
        ..
    } = query_service
        .get_block(block_id.0)
        .await
        .map_err(server_error)?;

    // Extract the transaction and associated data from the block.
    if tx_id >= block.block.0.len() {
        return Err(tide::Error::from_str(
            StatusCode::BadRequest,
            "invalid transaction id",
        ));
    }
    let tx = block.block.0.swap_remove(tx_id);
    let proofs = block.proofs.swap_remove(tx_id);
    let uids = uids.swap_remove(tx_id);
    let (memos, sig) = match memos.swap_remove(tx_id) {
        Some((memos, sig)) => (Some(memos), Some(sig)),
        None => (None, None),
    };

    Ok(CommittedTransaction {
        id: TransactionId(block_id, tx_id),
        data: tx,
        proofs,
        output_uids: uids,
        output_memos: memos,
        memos_signature: sig,
    })
}

async fn get_unspent_record(
    bindings: &HashMap<String, RouteBinding>,
    query_service: &impl QueryService,
) -> Result<UnspentRecord, tide::Error> {
    if bindings[":mempool"].value.as_boolean()? {
        return Err(tide::Error::from_str(
            StatusCode::NotImplemented,
            "mempool queries unimplemented",
        ));
    }

    let TransactionId(block_id, tx_id) = bindings[":txid"].value.to()?;
    let output_index = bindings[":output_index"].value.as_index()?;

    // First get the block containing the transaction.
    let LedgerTransition {
        block, memos, uids, ..
    } = query_service
        .get_block(block_id.0)
        .await
        .map_err(server_error)?;

    // Extract the transaction and associated data from the block.
    if tx_id >= block.block.0.len() {
        return Err(tide::Error::from_str(
            StatusCode::BadRequest,
            "invalid transaction id",
        ));
    }
    let tx = &block.block.0[tx_id];

    // Extract data about the requested output from the transaction.
    if output_index >= tx.output_len() {
        return Err(tide::Error::from_str(
            StatusCode::BadRequest,
            "invalid output index",
        ));
    }
    let comm = tx.output_commitments()[output_index];
    let uid = uids[tx_id][output_index];
    let memo = memos[tx_id]
        .as_ref()
        .map(|(memos, _sig)| memos[output_index].clone());
    Ok(UnspentRecord {
        commitment: comm,
        uid,
        memo,
    })
}

async fn subscribe(
    req: tide::Request<WebState>,
    bindings: &HashMap<String, RouteBinding>,
) -> Result<tide::Response, tide::Error> {
    let index = bindings[":index"].value.as_index()? as u64;
    Ok(sse::upgrade(req, move |req, sender| async move {
        let mut events = req.state().query_service.subscribe(index).await;
        while let Some(event) = events.next().await {
            sender
                .send(
                    event.as_static(),
                    serde_json::to_string(&event).map_err(server_error)?,
                    None,
                )
                .await?;
        }
        Ok(())
    }))
}

fn respond<T: Serialize>(content_type: mime::Mime, res: T) -> Result<tide::Response, tide::Error> {
    match content_type.subtype() {
        "json" => Ok(tide::Response::from(json!(res))),
        _ => Err(tide::Error::from_str(
            StatusCode::UnsupportedMediaType,
            format!("unsupported content type {}", content_type),
        )),
    }
}

pub async fn dispatch_url(
    req: tide::Request<WebState>,
    route_pattern: &str,
    bindings: &HashMap<String, RouteBinding>,
) -> Result<tide::Response, tide::Error> {
    let first_segment = route_pattern
        .split_once('/')
        .unwrap_or((route_pattern, ""))
        .0;
    let key = ApiRouteKey::from_str(first_segment).expect("Unknown route");
    //todo !jeb.bearer Get the content type to respond with from the request (URL parameter or
    // header or something). All endpoint functions return a serializable structure, so we can
    // respond with any content type that serde can target.
    let res_type = mime::JSON;
    match key {
        ApiRouteKey::getblock => respond(
            res_type,
            get_block(bindings, &req.state().query_service).await?,
        ),
        ApiRouteKey::getblockcount => {
            respond(res_type, get_block_count(&req.state().query_service).await?)
        }
        ApiRouteKey::getblockhash => respond(
            res_type,
            get_block_hash(bindings, &req.state().query_service).await?,
        ),
        ApiRouteKey::getblockid => respond(
            res_type,
            get_block_id(bindings, &req.state().query_service).await?,
        ),
        ApiRouteKey::getinfo => respond(res_type, get_info(&req.state().query_service).await?),
        ApiRouteKey::getmempool => dummy_url_eval(route_pattern, bindings),
        ApiRouteKey::gettransaction => respond(
            res_type,
            get_transaction(bindings, &req.state().query_service).await?,
        ),
        ApiRouteKey::getunspentrecord => respond(
            res_type,
            get_unspent_record(bindings, &req.state().query_service).await?,
        ),
        ApiRouteKey::getunspentrecordsetinfo => dummy_url_eval(route_pattern, bindings),
        ApiRouteKey::subscribe => subscribe(req, bindings).await,
    }
}