//! `moa daemon` drops its runtime as soon as `run` returns. A client whose op was in flight
//! at shutdown must still get its line then, so `run` has to wait for the sessions.

mod common;

use std::time::Duration;

use serde_json::json;
use tokio::time::timeout;

use common::*;

#[test]
fn a_reply_in_flight_survives_the_runtime_being_dropped() {
    for round in 0..3 {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .unwrap();
        let (bs, d) = rt.block_on(async {
            let d = boot().await;
            // Many clients with an op in flight, so many sessions have a write to make as
            // the daemon ends.
            let mut bs: Vec<Client> = (0..20)
                .map(|_| {
                    let mut b = Client::connect(&d.sock);
                    b.hello();
                    b
                })
                .collect();
            let mut a = Client::connect(&d.sock);
            a.hello();
            for (i, b) in bs.iter_mut().enumerate() {
                b.write(&request(
                    90,
                    "group.add",
                    json!({"path": format!("late{round}-{i}")}),
                ));
            }
            assert_eq!(a.call("shutdown", json!({}))["result"], json!({}));
            let mut d = d;
            let ended = timeout(Duration::from_secs(10), &mut d.done)
                .await
                .expect("run ends within 10 s")
                .expect("no panic");
            assert!(ended.is_ok(), "{ended:?}");
            (bs, d)
        });
        // The runtime goes now, as in `moa daemon`.
        drop(rt);
        for mut b in bs {
            let first = b.read().expect("a reply or the stopping line, not EOF");
            assert_eq!(first["id"], json!(90), "{first}");
        }
        drop(d);
    }
}
