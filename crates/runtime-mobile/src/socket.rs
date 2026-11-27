//! `READY`, which Phoenix's gateway session applies itself and never passes
//! through the registry. It is read off the host's `@ws` events, where Java
//! hands Rust each inflated gateway message.

use std::sync::OnceLock;

use memchr::memmem;
use phoenix_runtime::gateway;
use phoenix_sigs::registry::EventId;
use phoenix_wire::{
   DispatchEvent,
   decode_dispatch,
   edit_frames,
};
use serde::Deserialize;
use serde_json::value::RawValue;

use crate::{
   bridge::{
      Action,
      Sink,
   },
   host::Event,
};

/// Discord's gateway JSON is compact, so a READY message holds these bytes
/// and every other frame is skipped with one scan.
const READY_TAG: &[u8] = br#""t":"READY""#;

#[derive(Deserialize)]
struct Message<'a> {
   #[serde(borrow)]
   t: Option<&'a str>,
   #[serde(borrow)]
   d: Option<&'a RawValue>,
}

pub struct Socket;

impl Sink for Socket {
   fn ops(&self, _batch: &[u8]) -> Action {
      Action::Pass
   }

   fn dispatch(&self, frame: &[u8]) -> Action {
      static READY: OnceLock<Option<EventId>> = OnceLock::new();

      let Some(ready) = *READY.get_or_init(|| gateway::event("READY")) else {
         return Action::Pass;
      };
      if !gateway::listened(ready) || memmem::find(frame, READY_TAG).is_none() {
         return Action::Pass;
      }
      let Ok(items) = decode_dispatch(frame) else {
         return Action::Pass;
      };
      // Before the host names load, the body alone tells a gateway message.
      let ws = crate::host::table().and_then(|host| host.event(Event::Ws));
      let edits = items
         .iter()
         .filter_map(|item| {
            let DispatchEvent::Host { event, body, .. } = item.event else {
               return None;
            };
            if ws.is_some_and(|number| event != number) {
               return None;
            }
            let message = serde_json::from_slice::<Message<'_>>(body).ok()?;
            let data = message.d.filter(|_| message.t == Some("READY"))?.get();
            let json = gateway::intercept(ready, data)?;
            let start = data.as_ptr().addr() - body.as_ptr().addr();
            let head = &frame[item.offset..item.offset + item.len - body.len()];
            let mut out = Vec::with_capacity(head.len() + body.len() - data.len() + json.len());
            out.extend_from_slice(head);
            out.extend_from_slice(&body[..start]);
            out.extend_from_slice(json.as_bytes());
            out.extend_from_slice(&body[start + data.len()..]);
            Some((item.offset, Some(out)))
         })
         .collect::<Vec<_>>();
      if edits.is_empty() {
         return Action::Pass;
      }
      Action::Replace(edit_frames(frame, &edits))
   }
}
