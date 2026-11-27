//! What the debug shell shows of Phoenix's mobile bridge: host calls and
//! events, view events, decoded UI-op batches and the mirrored view tree.

use phoenix_runtime::remote::{
   self,
   Done,
   Reply,
   Request,
   Ring,
};
use phoenix_ui_mobile::{
   Op,
   Prop,
   inspect::WireJson,
};
use serde::Serialize;

static OPS: Ring = Ring::new(64);

/// A bridge event for the debug event log. Never put HTTP headers, bodies,
/// message content or credentials in one.
#[derive(Serialize)]
#[serde(tag = "source", rename_all = "camelCase")]
pub enum Trace<'a> {
   HostCall {
      module: u16,
      method: u16,
   },
   HostEvent {
      event: u16,
   },
   View {
      node:    i32,
      event:   Prop,
      aid:     Option<&'a str>,
      payload: WireJson<'a>,
   },
}

/// Records a decoded UI-op batch when the debug shell is on.
pub fn record_ops(ops: &[Op]) {
   #[derive(Serialize)]
   struct Batch<'a> {
      count: usize,
      ops:   &'a [Op],
   }

   if !ops.is_empty() {
      OPS.push(&Batch {
         count: ops.len(),
         ops,
      });
   }
}

#[cfg(target_os = "android")]
pub const ROUTES: &str = ", GET /ops[?match=&limit=], GET /tree[/<id>][?depth=], POST /update/self";
#[cfg(target_os = "ios")]
pub const ROUTES: &str = ", GET /ops[?match=&limit=], GET /tree[/<id>][?depth=]";

pub fn route(request: &Request) -> Option<Reply> {
   let depth = || {
      remote::query_param(&request.query, "depth")
         .and_then(|depth| depth.parse::<usize>().ok())
         .unwrap_or(6)
   };
   let reply = match (request.method.as_str(), request.path.as_str()) {
      ("GET", "/ops") => OPS.recent(&request.query, 20),
      ("POST", "/clear") => {
         OPS.clear();
         remote::clear_events();
         remote::ok_json(&Done::Cleared)
      },
      ("GET", "/tree") => tree(None, depth()),
      ("GET", path) if let Some(id) = path.strip_prefix("/tree/") => {
         let Ok(id) = id.parse::<i32>() else {
            return Some(remote::err("400 Bad Request", "invalid node id"));
         };
         tree(Some(id), depth())
      },
      #[cfg(target_os = "android")]
      ("POST", "/update/self") => {
         crate::loader::self_update_from_site();
         remote::ok_json(&Done::Started)
      },
      _ => return None,
   };
   Some(reply)
}

fn tree(root: Option<i32>, depth: usize) -> Reply {
   crate::patches::with_tree(root, depth, |tree| {
      tree.map_or_else(
         || remote::err("404 Not Found", "root node not seen yet"),
         |tree| remote::ok_json(&tree),
      )
   })
}
