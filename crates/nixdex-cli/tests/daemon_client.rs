//! End-to-end test of the daemon HTTP server and the CLI's thin client.
//!
//! Boots `nixdex-core`'s daemon in-process against a synthetic database and
//! queries it through `nixdex_cli::daemon_client::DaemonClient` over real HTTP,
//! exercising the same `/nix-locate` path the CLI uses.

use std::path::Path;
use std::time::Duration;

use bytes::Bytes;
use nixdex_cli::daemon_client::DaemonClient;
use nixdex_core::daemon::{DaemonConfig, IndexCacheMode};
use nixdex_core::database::Writer;
use nixdex_core::files::FileTree;
use nixdex_core::store_path::{Origin, StorePath};

fn build_synthetic_db(path: &Path) {
    let mut writer = Writer::create(path, 3).expect("create synthetic database");

    for pkg in 0..50u32 {
        let name = format!("pkg{pkg}-1.0");
        let hash = format!("{pkg:032x}");
        let store_path = StorePath::new(
            String::from("/nix/store"),
            hash,
            name,
            Origin {
                attr: format!("pkg{pkg}"),
                output: String::from("out"),
                toplevel: true,
                system: Some(String::from("x86_64-linux")),
            },
        );

        let mut entries = Vec::new();
        entries.push((Bytes::from_static(b"ls"), FileTree::regular(0, true)));
        let tree = FileTree::directory(vec![(
            Bytes::from_static(b"bin"),
            FileTree::directory(entries),
        )]);
        writer.add(&store_path, &tree, b"").expect("add package");
    }

    writer.finish().expect("finish database");
}

#[tokio::test]
async fn daemon_serves_locate_over_http() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("files");
    build_synthetic_db(&db_path);

    // Bind to port 0 to obtain a free ephemeral port.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind free port");
    let addr = listener.local_addr().expect("local addr");
    drop(listener);
    let addr_str = addr.to_string();

    let config = DaemonConfig {
        prebuilt: nixdex_core::prebuilt::PrebuiltConfig::default(),
        http_addr: addr_str.clone(),
        local_database: Some(dir.path().to_path_buf()),
        local_refresh_interval: Duration::from_secs(3600),
        admin_token: None,
        index_cache_mode: IndexCacheMode::Resident,
    };

    tokio::spawn(async move {
        let _ = nixdex_core::daemon::run(&config).await;
    });

    let client = DaemonClient::new(addr_str);

    // Wait for the daemon to become ready (loads + builds sidecars).
    let ready = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if client.ready().await {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("daemon became ready");
    assert!(ready, "daemon should report ready");

    let query = vec![
        ("pattern".to_string(), "bin/ls".to_string()),
        ("regex".to_string(), "false".to_string()),
    ];
    let response = client
        .locate(&query)
        .await
        .expect("locate request succeeds");

    assert!(
        response
            .matches
            .iter()
            .any(|m| m.path.as_deref() == Some("/bin/ls")),
        "daemon should return the /bin/ls entry; got {:?}",
        response.matches
    );
}

/// A daemon with an admin token answers only authenticated requests.
///
/// `DaemonClient` sent no `Authorization` header at all, so every request to
/// such a daemon came back 401 and daemon-backed searches always failed.
#[tokio::test]
async fn daemon_locate_carries_the_admin_token() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("files");
    build_synthetic_db(&db_path);

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind free port");
    let addr = listener.local_addr().expect("local addr");
    drop(listener);
    let addr_str = addr.to_string();

    let token = String::from("s3cret-token");
    let config = DaemonConfig {
        prebuilt: nixdex_core::prebuilt::PrebuiltConfig::default(),
        http_addr: addr_str.clone(),
        local_database: Some(dir.path().to_path_buf()),
        local_refresh_interval: Duration::from_secs(3600),
        admin_token: Some(token.clone()),
        index_cache_mode: IndexCacheMode::Resident,
    };

    tokio::spawn(async move {
        let _ = nixdex_core::daemon::run(&config).await;
    });

    let authenticated = DaemonClient::with_token(addr_str.clone(), Some(token));
    let anonymous = DaemonClient::with_token(addr_str, None);

    let ready = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if authenticated.ready().await {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("daemon became ready");
    assert!(ready, "the authenticated client should reach /ready");

    // `/ready` sits outside the auth layer on purpose, so it answers either
    // way. That is what made this bug quiet: the CLI saw a healthy daemon and
    // then had every data request refused.
    assert!(
        anonymous.ready().await,
        "the readiness probe is deliberately unauthenticated"
    );

    let query = vec![
        ("pattern".to_string(), "bin/ls".to_string()),
        ("regex".to_string(), "false".to_string()),
    ];

    let response = authenticated
        .locate(&query)
        .await
        .expect("an authenticated locate succeeds");
    assert!(
        response
            .matches
            .iter()
            .any(|m| m.path.as_deref() == Some("/bin/ls")),
        "the authenticated locate should return /bin/ls; got {:?}",
        response.matches
    );

    assert!(
        anonymous.locate(&query).await.is_err(),
        "an unauthenticated locate must fail rather than return data"
    );
}
