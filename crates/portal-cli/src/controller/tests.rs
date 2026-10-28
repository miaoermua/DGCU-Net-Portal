use super::reconnect::ReconnectCheckpoint;
use super::*;
use crate::tests::row;

#[test]
fn binds_unique_session_after_accounting_delay() {
    let mut c = Controller {
        awaiting_session: Some(["old".to_owned()].into_iter().collect()),
        rows: vec![row("old", 60, 0, 0)],
        ..Default::default()
    };
    c.bind_new_session();
    assert!(c.selected.is_none());
    assert!(c.awaiting_session.is_some());
    c.rows.push(row("new", 1, 0, 0));
    c.bind_new_session();
    assert_eq!(c.selected.as_deref(), Some("new"));
    assert!(c.awaiting_session.is_none());
}
#[test]
fn multiple_new_sessions_require_explicit_selection() {
    let mut c = Controller {
        awaiting_session: Some(HashSet::new()),
        rows: vec![row("a", 1, 0, 0), row("b", 1, 0, 0)],
        ..Default::default()
    };
    c.bind_new_session();
    assert!(c.selected.is_none());
    assert!(c.awaiting_session.is_none());
}
fn context(ipv4: &str) -> network::NetworkContext {
    network::NetworkContext {
        interface_name: "en0".into(),
        ipv4: ipv4.into(),
        mac: "02:00:00:00:00:01".into(),
    }
}
/// 合盖唤醒后网卡地址变了：client 必须按新地址重建，且不丢后台 Cookie。
/// 这是这次"日志疯狂刷 network.error"故障的核心回归测试。
#[tokio::test]
async fn changed_interface_address_rebuilds_client_without_relogin() {
    let mock = crate::tests::Mock::new("direct");
    let mut c = Controller::default();
    let client = PortalClient::new(&mock.base).unwrap();
    client.login("test-user", "test-password").await.unwrap();
    c.api = Some(client);
    c.bound_network = Some(context("192.0.2.1"));
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = calls.clone();
    c.resolve_network = Arc::new(move |_| {
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(Some(context("127.0.0.1")))
    });
    assert!(c.rebind_changed_network());
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    let api = c.api.as_ref().unwrap();
    assert_eq!(
        api.bound_address(),
        Some(IpAddr::from([127, 0, 0, 1])),
        "应按新地址重新绑定"
    );
    // Cookie 还在，所以不需要重新登录就能继续查询。
    assert!(api.sessions().await.is_ok());
    // 地址没再变化就不该反复重建。
    assert!(!c.rebind_changed_network());
}
/// 网卡地址与后台会话的对应关系决定「自动选中本机会话」。
///
/// 同一 IP 上有两条会话时代码不能替用户挑，必须留空等手动选：挑错就等于
/// 把别人的设备下线或流量算到自己头上。
#[test]
fn local_session_binding_needs_an_unambiguous_ip_match() {
    // `tests::row` 造出的会话都带 IP 192.0.2.2，网卡上下文要对上才能匹配。
    let network = context("192.0.2.2");
    let rows = vec![crate::tests::row("a", 1, 0, 0)];
    assert_eq!(
        Controller::unique_local_session(&rows, Some(&network)),
        Some("a".into())
    );
    // 没有网卡上下文时无从判断，一律不自动绑定。
    assert_eq!(Controller::unique_local_session(&rows, None), None);
    // IP 对不上时不绑定。
    assert_eq!(
        Controller::unique_local_session(&rows, Some(&context("192.0.2.99"))),
        None
    );
    // 同一 IP 出现两条：不猜。
    let ambiguous = vec![
        crate::tests::row("a", 1, 0, 0),
        crate::tests::row("b", 1, 0, 0),
    ];
    assert_eq!(
        Controller::unique_local_session(&ambiguous, Some(&network)),
        None
    );
}
/// 连不上 portal 服务器时，文案要指出「不在校园网」和「服务器离线」这两个方向。
///
/// 后台与 Portal 都在校园内网地址上，这两种情况占绝大多数；只说「请检查网络」
/// 会让用户无从下手，所以这句是防呆设计的一部分，锁住以免被改回含糊的说法。
#[test]
fn unreachable_portal_names_the_two_likely_causes() {
    use crate::NetworkFault;
    for fault in [
        NetworkFault::Connect,
        NetworkFault::Timeout,
        NetworkFault::Other,
    ] {
        let message = AppError::Network(fault).to_string();
        assert!(message.contains("校园网"), "{message}");
        assert!(message.contains("离线"), "{message}");
    }
}
/// 轮询失败要能区分"环境暂时不可用"和"认证被拒"。
#[test]
fn classifies_poll_failures_for_automatic_recovery() {
    use crate::NetworkFault;
    assert!(Controller::recoverable_poll_error(&AppError::Network(
        NetworkFault::Connect
    )));
    assert!(Controller::recoverable_poll_error(&AppError::Rejected));
    assert!(!Controller::recoverable_poll_error(&AppError::InvalidUrl));
    assert!(Controller::retryable_connection_error(
        &AppError::NetworkInterface("no interface".into())
    ));
    assert!(!Controller::retryable_connection_error(&AppError::Rejected));
}
/// 网络/网卡不可用时的重连尝试必须可回滚：保住凭据和现有会话，只推进退避。
#[tokio::test]
async fn network_failure_keeps_credential_and_backs_off() {
    let mut c = Controller {
        settings: Settings {
            credential_store: CredentialStore::System,
            reconnect_mode: ReconnectMode::NewSession,
            ..Default::default()
        },
        // 已连线状态下后台轮询才会跑；默认构造是暂停态。
        paused: false,
        ..Default::default()
    };
    c.credential = Some(Credential::new("test-user".into(), "test-password".into()));
    c.resolve_network = Arc::new(|_| Err("no interface".into()));
    c.retry_after_failure(|_| {}).await;
    assert!(c.credential.is_some(), "凭据必须保留，否则永远无法自愈");
    assert!(!c.paused, "网卡暂时消失不该像认证被拒那样停下来");
    assert_eq!(c.network_attempts, 1);
    assert!(c.last_attempt.is_some());
    // 退避窗口内不再重复尝试。
    c.retry_after_failure(|_| {}).await;
    assert_eq!(c.network_attempts, 1);
}
/// 后台地址或网卡设置变了，已建立的连接要跟着变。
#[test]
fn settings_change_invalidates_existing_binding() {
    let mut c = Controller {
        bound_network: Some(context("192.0.2.1")),
        ..Default::default()
    };
    let settings = Settings {
        interface_name: "en1".into(),
        ..Default::default()
    };
    c.apply_settings(settings);
    assert!(c.bound_network.is_none(), "换网卡后应强制重新解析并重建");
}
/// 仅内存模式只是不落盘，不是"不能重连"：开了重连就必须在进程内保留凭据。
#[tokio::test]
async fn memory_login_keeps_credential_only_when_redial_is_enabled() {
    for (mode, retained) in [
        (ReconnectMode::NewSession, true),
        (ReconnectMode::TerminateAndReconnect, true),
        (ReconnectMode::Disabled, false),
    ] {
        let mock = crate::tests::Mock::new("direct");
        let mut c = Controller {
            settings: Settings {
                server: mock.base.clone(),
                credential_store: CredentialStore::Memory,
                reconnect_mode: mode,
                ..Default::default()
            },
            ..Default::default()
        };
        c.resolve_network = Arc::new(|_| Ok(Some(context("127.0.0.1"))));
        c.connect(
            Credential::new("test-user".into(), "test-password".into()),
            "",
            false,
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(c.credential.is_some(), retained, "{mode:?}");
        assert_eq!(c.paused, mode == ReconnectMode::Disabled, "{mode:?}");
        if retained {
            assert_eq!(c.credential.as_ref().unwrap().password, "test-password");
        }
    }
}
/// 会话消失只该累计缺失次数；重连开着时 API 与凭据要留到重拨真正发生。
#[test]
fn memory_session_loss_keeps_reconnect_state_when_redial_is_enabled() {
    let mut c = Controller {
        settings: Settings {
            credential_store: CredentialStore::Memory,
            reconnect_mode: ReconnectMode::NewSession,
            ..Default::default()
        },
        api: Some(PortalClient::new("http://127.0.0.1:9/lfradius/").unwrap()),
        credential: Some(Credential::new("test-user".into(), "test-password".into())),
        selected: Some("gone".into()),
        paused: false,
        ..Default::default()
    };
    for _ in 0..3 {
        c.update_session_status();
    }
    assert_eq!(c.missing, 3);
    assert!(c.api.is_some(), "重拨还需要这个 client");
    assert!(c.credential.is_some(), "重拨还需要这份进程内凭据");
    assert_eq!(c.status, "offline");
}
/// 重连关着时这份凭据没有任何后续用途，会话消失就该立刻释放。
#[test]
fn memory_session_loss_releases_state_when_redial_is_disabled() {
    let mut c = Controller {
        settings: Settings {
            credential_store: CredentialStore::Memory,
            reconnect_mode: ReconnectMode::Disabled,
            ..Default::default()
        },
        api: Some(PortalClient::new("http://127.0.0.1:9/lfradius/").unwrap()),
        credential: Some(Credential::new("test-user".into(), "test-password".into())),
        selected: Some("gone".into()),
        ..Default::default()
    };
    c.update_session_status();
    assert!(c.api.is_none());
    assert!(c.credential.is_none());
    assert!(c.paused);
    assert_eq!(c.missing, 0);
}
/// 三次缺失检测后必须真的发起重拨，并且用的是内存里留下的凭据。
#[tokio::test]
async fn memory_redial_fires_after_three_missing_checks() {
    let mock = crate::tests::Mock::new("direct");
    let settings = Settings {
        server: mock.base.clone(),
        credential_store: CredentialStore::Memory,
        reconnect_mode: ReconnectMode::NewSession,
        ..Default::default()
    };
    let api = PortalClient::new(&settings.server).unwrap();
    api.login("test-user", "test-password").await.unwrap();
    let mut c = Controller {
        settings,
        api: Some(api),
        credential: Some(Credential::new("test-user".into(), "test-password".into())),
        selected: Some("gone".into()),
        paused: false,
        ..Default::default()
    };
    c.resolve_network = Arc::new(|_| Ok(Some(context("127.0.0.1"))));
    c.redial_tick(|_| {}).await;
    c.redial_tick(|_| {}).await;
    assert_eq!(c.missing, 2);
    assert_eq!(c.attempts, 0, "未到三次缺失不应重拨");
    assert!(c.credential.is_some());
    c.redial_tick(|_| {}).await;
    assert_eq!(c.attempts, 1);
    assert!(
        mock.requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.contains("cmcc_login_value")),
        "第三次缺失后应重新走一遍 Portal 登录"
    );
    assert!(c.credential.is_some(), "重拨后凭据仍在进程内");
    // 短时间内重复轮询会落在退避窗口里，不会追加尝试。
    c.redial_tick(|_| {}).await;
    assert_eq!(c.attempts, 1, "退避窗口内不应重复拨号");
}
/// 认证被拒后暂停自动重拨：不再自动尝试，但凭据留着手动重试。
#[tokio::test]
async fn rejected_redial_pauses_and_keeps_credential() {
    let mock = crate::tests::Mock::new("fake");
    let settings = Settings {
        server: mock.base.clone(),
        credential_store: CredentialStore::Memory,
        reconnect_mode: ReconnectMode::NewSession,
        ..Default::default()
    };
    let api = PortalClient::new(&settings.server).unwrap();
    api.login("test-user", "test-password").await.unwrap();
    let mut c = Controller {
        settings,
        api: Some(api),
        credential: Some(Credential::new("test-user".into(), "test-password".into())),
        selected: Some("gone".into()),
        paused: false,
        ..Default::default()
    };
    c.resolve_network = Arc::new(|_| Ok(Some(context("127.0.0.1"))));
    for _ in 0..3 {
        c.redial_tick(|_| {}).await;
    }
    assert_eq!(c.attempts, 1);
    assert!(c.paused, "认证被拒应暂停自动重拨");
    assert!(c.message.contains("已暂停"), "{}", c.message);
    assert!(c.credential.is_some(), "暂停不等于释放，凭据留给手动重试");
    c.redial_tick(|_| {}).await;
    assert_eq!(c.attempts, 1);
}
/// 手动下线与 Forget 都是显式释放：不论凭据存在哪里都必须作废。
#[tokio::test]
async fn manual_release_paths_clear_credentials() {
    let mut c = Controller {
        settings: Settings {
            credential_store: CredentialStore::Memory,
            reconnect_mode: ReconnectMode::NewSession,
            ..Default::default()
        },
        credential: Some(Credential::new("test-user".into(), "test-password".into())),
        ..Default::default()
    };
    // 没有可用 client 时下线会失败，但手动下线本身就该释放凭据。
    assert!(c.disconnect("gone").await.is_err());
    assert!(c.credential.is_none());
    assert!(c.paused);
    c.credential = Some(Credential::new("test-user".into(), "test-password".into()));
    c.forget();
    assert!(c.credential.is_none());
    assert!(c.api.is_none());
}
/// 关掉重连后，进程内不再保留没有用途的凭据。
#[test]
fn disabling_redial_drops_retained_credential() {
    let mut c = Controller {
        settings: Settings {
            credential_store: CredentialStore::Memory,
            reconnect_mode: ReconnectMode::NewSession,
            ..Default::default()
        },
        ..Default::default()
    };
    c.credential = Some(Credential::new("test-user".into(), "test-password".into()));
    c.apply_settings(Settings {
        credential_store: CredentialStore::Memory,
        reconnect_mode: ReconnectMode::Disabled,
        ..Default::default()
    });
    assert!(c.credential.is_none());
}
/// 认证成功、恰好新增一条会话：自动绑定，并给出不带附加条件的成功提示。
#[tokio::test]
async fn portal_success_adopts_a_single_new_session() {
    let mock = crate::tests::Mock::new("direct");
    mock.script_sessions(vec![
        vec![],
        vec![crate::tests::session_json("new", "192.0.2.2")],
    ]);
    let mut c = Controller {
        settings: Settings {
            server: mock.base.clone(),
            ..Default::default()
        },
        ..Default::default()
    };
    c.resolve_network = Arc::new(|_| Ok(Some(context("127.0.0.1"))));
    let snap = c
        .connect(
            Credential::new("test-user".into(), "test-password".into()),
            "",
            false,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(snap.selected_id.as_deref(), Some("new"));
    assert!(snap.authenticated);
    assert_eq!(snap.message, "Portal 已确认成功");
    assert!(snap.rates.is_empty() || snap.rates.contains_key("new"));
}
/// 认证成功、但新增两条会话：认证状态保留，绝不替用户挑一条。
///
/// 挑错的代价是流量显示成别人的，更糟的是 `update_session_status` 会把
/// 别人的掉线当成自己掉线，进而触发自愈重连去动其他设备。
#[tokio::test]
async fn portal_success_with_multiple_new_sessions_asks_for_a_choice() {
    let mock = crate::tests::Mock::new("direct");
    mock.script_sessions(vec![
        vec![],
        vec![
            crate::tests::session_json("a", "192.0.2.2"),
            crate::tests::session_json("b", "192.0.2.3"),
        ],
    ]);
    let mut c = Controller {
        settings: Settings {
            server: mock.base.clone(),
            reconnect_mode: ReconnectMode::NewSession,
            ..Default::default()
        },
        ..Default::default()
    };
    c.resolve_network = Arc::new(|_| Ok(Some(context("127.0.0.1"))));
    let snap = c
        .connect(
            Credential::new("test-user".into(), "test-password".into()),
            "",
            false,
            |_| {},
        )
        .await
        .unwrap();
    assert!(snap.authenticated, "认证本身是成功的，不能被报成失败");
    assert_eq!(snap.selected_id, None, "多条新增时不得自动选第一条");
    assert!(
        snap.message.contains("尚未绑定管理会话"),
        "提示要说清没绑定：{}",
        snap.message
    );
    assert!(!snap.message.contains("失败"), "{}", snap.message);
    // 没有绑定就没有"本机会话消失"可言，缺失计数必须留在 0，
    // 否则会跨过 redial_tick 的 missing >= 3 阈值去重连。
    assert_eq!(c.missing, 0);
}
/// 认证成功、会计系统还没上报任何会话：保留成功状态，等后续轮询绑定。
#[tokio::test]
async fn portal_success_without_new_sessions_keeps_success() {
    let mock = crate::tests::Mock::new("direct");
    mock.script_sessions(vec![vec![], vec![]]);
    let mut c = Controller {
        settings: Settings {
            server: mock.base.clone(),
            ..Default::default()
        },
        ..Default::default()
    };
    c.resolve_network = Arc::new(|_| Ok(Some(context("127.0.0.1"))));
    let snap = c
        .connect(
            Credential::new("test-user".into(), "test-password".into()),
            "",
            false,
            |_| {},
        )
        .await
        .unwrap();
    assert!(snap.authenticated, "没有新会话不等于认证失败");
    assert_eq!(snap.selected_id, None, "也不能伪造已绑定状态");
    assert!(
        snap.message.contains("尚未绑定管理会话"),
        "{}",
        snap.message
    );
    // 保持等待态：后续轮询一旦看到唯一新增会话就能自动绑定。
    assert!(c.awaiting_session.is_some());
}
/// 仅登录后台（不认证）：沿用原有提示，要求用户自己选会话。
#[tokio::test]
async fn backend_only_login_asks_for_a_session_choice() {
    let mock = crate::tests::Mock::new("direct");
    mock.script_sessions(vec![
        vec![],
        vec![crate::tests::session_json("a", "192.0.2.2")],
    ]);
    let mut c = Controller {
        settings: Settings {
            server: mock.base.clone(),
            ..Default::default()
        },
        ..Default::default()
    };
    c.resolve_network = Arc::new(|_| Ok(Some(context("127.0.0.1"))));
    let snap = c
        .connect(
            Credential::new("test-user".into(), "test-password".into()),
            "",
            true,
            |_| {},
        )
        .await
        .unwrap();
    assert!(snap.authenticated);
    assert_eq!(snap.selected_id, None);
    assert_eq!(snap.message, "已登录用户后台；请选择要管理的会话");
}
/// 记录现有优先级：登录后台时就能按本机地址认出会话，会直接进
/// `session_online` 并返回，不进入 Portal 认证与后续绑定流程。
///
/// 因此"先按本机 IP 认领、再被新增会话覆盖"这条路径在到达 `bind_new_session`
/// 之前就短路了；这里用测试固定住这个行为，避免以后修改提示时顺手改了绑定次序。
#[tokio::test]
async fn restored_local_session_short_circuits_before_binding() {
    let mock = crate::tests::Mock::new("direct");
    mock.script_sessions(vec![vec![crate::tests::session_json("local", "127.0.0.1")]]);
    let mut c = Controller {
        settings: Settings {
            server: mock.base.clone(),
            reconnect_mode: ReconnectMode::NewSession,
            ..Default::default()
        },
        ..Default::default()
    };
    c.resolve_network = Arc::new(|_| Ok(Some(context("127.0.0.1"))));
    let snap = c
        .connect(
            Credential::new("test-user".into(), "test-password".into()),
            "",
            false,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(snap.selected_id.as_deref(), Some("local"));
    assert_eq!(snap.status, "session_online");
    assert_eq!(snap.message, "已恢复本机对应的在线会话");
}
/// 自动重拨（`reconnecting = true`）不会走 `clear()`；成功恢复本机会话时
/// 上一轮的丢失结论必须一起失效，不能让旧告警盖住"已恢复"。
#[tokio::test]
async fn restored_local_session_clears_the_previous_loss() {
    let mock = crate::tests::Mock::new("direct");
    mock.script_sessions(vec![vec![crate::tests::session_json("local", "127.0.0.1")]]);
    let mut c = Controller {
        settings: Settings {
            server: mock.base.clone(),
            reconnect_mode: ReconnectMode::NewSession,
            ..Default::default()
        },
        reconnecting: true,
        loss: Some(SessionLoss {
            id: "local".into(),
            kind: LossKind::Unclear,
        }),
        ..Default::default()
    };
    c.resolve_network = Arc::new(|_| Ok(Some(context("127.0.0.1"))));
    let snap = c
        .connect(
            Credential::new("test-user".into(), "test-password".into()),
            "",
            false,
            |_| {},
        )
        .await
        .unwrap();
    assert!(c.loss.is_none(), "成功恢复后旧告警不能残留");
    assert!(!c.redial_hold, "恢复连接后不应继续压着重拨");
    assert_eq!(
        snap.message, "已恢复本机对应的在线会话",
        "快照不能被上一轮的丢失文案盖住"
    );
}
/// 认证后按本机 IP 认出了会话，同时后台又多出一条别的会话：
/// 新增的不止一条，不能把"本次认证产生的会话"猜成新增的那条。
#[tokio::test]
async fn restored_local_session_survives_an_extra_new_session() {
    let mock = crate::tests::Mock::new("direct");
    mock.script_sessions(vec![
        vec![],
        vec![
            crate::tests::session_json("local", "127.0.0.1"),
            crate::tests::session_json("other", "192.0.2.9"),
        ],
    ]);
    let mut c = Controller {
        settings: Settings {
            server: mock.base.clone(),
            reconnect_mode: ReconnectMode::NewSession,
            ..Default::default()
        },
        ..Default::default()
    };
    c.resolve_network = Arc::new(|_| Ok(Some(context("127.0.0.1"))));
    let snap = c
        .connect(
            Credential::new("test-user".into(), "test-password".into()),
            "",
            false,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(snap.selected_id.as_deref(), Some("local"));
    assert_eq!(snap.status, "accepted");
    assert_eq!(snap.message, "Portal 已确认成功");
    assert_eq!(c.missing, 0);
}
/// 没有选中会话时，即使 `missing` 已被其他路径推高，也不能自动重拨：
/// 空值不能被解释成"任选一条会话"。
#[tokio::test]
async fn unbound_controller_never_redials() {
    let mock = crate::tests::Mock::new("direct");
    let api = PortalClient::new(&mock.base).unwrap();
    api.login("test-user", "test-password").await.unwrap();
    let mut c = Controller {
        settings: Settings {
            server: mock.base.clone(),
            reconnect_mode: ReconnectMode::NewSession,
            ..Default::default()
        },
        api: Some(api),
        credential: Some(Credential::new("test-user".into(), "test-password".into())),
        selected: None,
        missing: 3,
        paused: false,
        bound_network: Some(context("127.0.0.1")),
        ..Default::default()
    };
    c.resolve_network = Arc::new(|_| Ok(Some(context("127.0.0.1"))));
    c.redial_tick(|_| {}).await;
    assert_eq!(c.attempts, 0, "没有选中会话就不该发起重拨");
    assert!(c.credential.is_some(), "更不能消费凭据去重连");
    assert!(
        mock.requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| !request.contains("cmcc_login_value")),
        "未绑定会话时不得走 Portal 认证"
    );
}
/// 没有绑定会话时，后台列表怎么变都不算"本机会话掉线"。
#[test]
fn unbound_controller_never_reports_a_drop() {
    let mut c = Controller {
        api: Some(PortalClient::new("http://127.0.0.1:9/lfradius/").unwrap()),
        rows: vec![row("a", 1, 0, 0), row("b", 1, 0, 0)],
        selected: None,
        status: "accepted".into(),
        ..Default::default()
    };
    for _ in 0..5 {
        c.update_session_status();
    }
    assert_eq!(c.missing, 0, "没有选中会话就不该累计缺失次数");
    assert_eq!(c.status, "accepted", "状态不能被改写成 offline");
    assert!(c.selected.is_none(), "更不能顺手绑定其中一条");
}
fn session(id: &str, ip: &str) -> OnlineSession {
    OnlineSession {
        radacctid: id.into(),
        username: "test-user".into(),
        acctstarttime: String::new(),
        acctsessiontime: 60,
        framedipaddress: Some(ip.parse().unwrap()),
        acctinputoctets: 0,
        acctoutputoctets: 0,
    }
}
/// 本机地址固定为 192.0.2.1，后台会话用别的地址，模拟"本机 IP 不在列表"。
fn observing(rows: Vec<OnlineSession>, selected: &str) -> Controller {
    Controller {
        rows,
        selected: Some(selected.into()),
        current_network: Some(context("192.0.2.1")),
        ..Default::default()
    }
}
fn crowded_elsewhere() -> Vec<OnlineSession> {
    vec![
        session("other-1", "203.0.113.11"),
        session("other-2", "203.0.113.12"),
        session("other-3", "203.0.113.13"),
    ]
}
/// 原会话还在，只是本机换了地址：不是丢失，绝不能提示被挤下线。
#[test]
fn changed_local_ip_with_session_present_is_not_a_loss() {
    let mut c = observing(vec![session("mine", "203.0.113.11")], "mine");
    c.update_session_status();
    assert_eq!(c.missing, 0);
    assert_eq!(c.status, "session_online");
    assert!(c.loss.is_none());
    assert!(!c.redial_hold);
}
/// 会话消失 + 本机 IP 不在列表 + 在线会话超过 2 条 + 连接异常 → 高疑似。
#[test]
fn suspected_eviction_needs_crowding_and_a_connection_fault() {
    let mut c = observing(crowded_elsewhere(), "mine");
    c.settings.reconnect_mode = ReconnectMode::NewSession;
    c.last_poll_failure = Some(Instant::now());
    c.update_session_status();
    assert_eq!(
        c.loss.as_ref().map(|loss| loss.kind),
        Some(LossKind::SuspectedEviction)
    );
    assert_eq!(c.loss.as_ref().unwrap().id, "mine");
    assert!(c.redial_hold, "高疑似时必须暂停自动抢线");
    assert!(
        c.loss_message(c.loss.as_ref().unwrap())
            .contains("疑似已被其他设备挤下线"),
        "{}",
        c.message
    );
    assert!(c.message.contains("自动重拨已暂停"), "{}", c.message);
}
/// 「连接异常」的证据只有两分钟时效：更早的失败不能再当作抢线依据。
///
/// 这一项用 `Instant` 而非墙上时钟，所以窗口判断不会被系统时间调整影响。
#[test]
fn poll_failure_evidence_expires_after_two_minutes() {
    let mut stale = observing(crowded_elsewhere(), "mine");
    stale.settings.reconnect_mode = ReconnectMode::NewSession;
    stale.last_poll_failure = Some(Instant::now() - Duration::from_secs(121));
    stale.update_session_status();
    assert_eq!(
        stale.loss.as_ref().map(|loss| loss.kind),
        Some(LossKind::Unclear),
        "超过两分钟的历史失败不能升级成疑似被挤下线"
    );
    assert!(!stale.redial_hold, "证据过期就不该停掉重拨");

    let mut fresh = observing(crowded_elsewhere(), "mine");
    fresh.settings.reconnect_mode = ReconnectMode::NewSession;
    fresh.last_poll_failure = Some(Instant::now() - Duration::from_secs(119));
    fresh.update_session_status();
    assert_eq!(
        fresh.loss.as_ref().map(|loss| loss.kind),
        Some(LossKind::SuspectedEviction),
        "窗口内的失败仍算证据"
    );
    assert!(fresh.redial_hold);
}
/// 后台一条会话都没上报地址时，不能断言"本机 IP 不在列表里"。
///
/// 那只是数据不全（`framedipaddress` 可以缺失），不是被挤下线的证据；
/// 把它当成 `Some(false)` 会让「地址缺失 + 后台抖动」这种无关组合误报抢占。
#[test]
fn sessions_without_reported_addresses_are_not_eviction_evidence() {
    let rows: Vec<OnlineSession> = ["other-1", "other-2", "other-3"]
        .iter()
        .map(|id| OnlineSession {
            radacctid: (*id).into(),
            username: "test-user".into(),
            acctstarttime: String::new(),
            acctsessiontime: 60,
            framedipaddress: None,
            acctinputoctets: 0,
            acctoutputoctets: 0,
        })
        .collect();
    let mut c = observing(rows, "mine");
    c.settings.reconnect_mode = ReconnectMode::NewSession;
    c.last_poll_failure = Some(Instant::now());
    c.update_session_status();
    assert_eq!(
        c.loss.as_ref().map(|loss| loss.kind),
        Some(LossKind::Unclear),
        "地址缺失只能算证据不足，不能升级成疑似被挤下线"
    );
    assert!(!c.redial_hold, "证据不足时不该停掉自动重拨");
}
/// 同样的丢失，但只剩两条会话：不能升级成多设备抢占。
#[test]
fn session_loss_with_two_sessions_stays_unclear() {
    let mut c = observing(
        vec![
            session("other-1", "203.0.113.11"),
            session("other-2", "203.0.113.12"),
        ],
        "mine",
    );
    c.last_poll_failure = Some(Instant::now());
    c.update_session_status();
    assert_eq!(
        c.loss.as_ref().map(|loss| loss.kind),
        Some(LossKind::Unclear)
    );
    assert!(!c.redial_hold, "证据不足时不应停掉重拨");
    assert!(c.message.contains("无法确认"), "{}", c.message);
    assert!(!c.message.contains("挤下线。"), "{}", c.message);
}
/// 只剩一条别人的会话，同样只是"原因不明"。
#[test]
fn session_loss_with_one_session_stays_unclear() {
    let mut c = observing(vec![session("other-1", "203.0.113.11")], "mine");
    c.last_poll_failure = Some(Instant::now());
    c.update_session_status();
    assert_eq!(
        c.loss.as_ref().map(|loss| loss.kind),
        Some(LossKind::Unclear)
    );
    assert!(!c.redial_hold);
}
/// 三条会话里包含本机 IP：抢占证据不成立，哪怕列表很热闹。
#[test]
fn crowded_list_with_matching_ip_is_not_eviction() {
    let mut c = observing(
        vec![
            session("other-1", "203.0.113.11"),
            session("other-2", "203.0.113.12"),
            session("same-ip-other-id", "192.0.2.1"),
        ],
        "mine",
    );
    c.last_poll_failure = Some(Instant::now());
    c.update_session_status();
    assert_eq!(
        c.loss.as_ref().map(|loss| loss.kind),
        Some(LossKind::Unclear)
    );
    assert!(!c.redial_hold);
}
/// 原会话还在、列表又超过 2 条：只是"设备多"，不是被挤。
#[test]
fn crowded_list_with_selected_session_present_never_alerts() {
    let mut rows = crowded_elsewhere();
    rows.push(session("mine", "192.0.2.1"));
    let mut c = observing(rows, "mine");
    c.last_poll_failure = Some(Instant::now());
    c.update_session_status();
    assert!(c.loss.is_none());
    assert_eq!(c.status, "session_online");
}
/// 解析不到本机 IP 时证据不足：只能给"原因不明"。
#[test]
fn unknown_local_ip_never_counts_as_eviction() {
    let mut c = observing(crowded_elsewhere(), "mine");
    c.current_network = None;
    c.last_poll_failure = Some(Instant::now());
    c.update_session_status();
    assert_eq!(
        c.loss.as_ref().map(|loss| loss.kind),
        Some(LossKind::Unclear)
    );
    assert!(!c.redial_hold);
}
/// 查询失败不能被当成"所有会话都消失了"。
#[tokio::test]
async fn failed_poll_does_not_claim_a_loss() {
    let mut c = Controller {
        settings: Settings {
            server: "http://127.0.0.1:9/lfradius/".into(),
            ..Default::default()
        },
        api: Some(PortalClient::new("http://127.0.0.1:9/lfradius/").unwrap()),
        selected: Some("mine".into()),
        bound_network: Some(context("192.0.2.1")),
        current_network: Some(context("192.0.2.1")),
        ..Default::default()
    };
    c.resolve_network = Arc::new(|_| Ok(Some(context("192.0.2.1"))));
    c.redial_tick(|_| {}).await;
    assert!(c.loss.is_none(), "请求失败时没有任何会话消失的证据");
    assert_eq!(c.missing, 0, "缺失计数只在成功列表上推进");
    assert!(c.last_poll_failure.is_some(), "失败要留作后续连接异常证据");
    assert!(!c.message.contains("消失"), "{}", c.message);
}
/// backend-only 登录会置 `paused = true`；监测不能再被它挡住。
#[tokio::test]
async fn paused_backend_only_session_still_monitors() {
    let mock = crate::tests::Mock::new("direct");
    let api = PortalClient::new(&mock.base).unwrap();
    api.login("test-user", "test-password").await.unwrap();
    let mut c = Controller {
        settings: Settings {
            server: mock.base.clone(),
            reconnect_mode: ReconnectMode::NewSession,
            ..Default::default()
        },
        api: Some(api),
        selected: Some("mine".into()),
        paused: true,
        bound_network: Some(context("192.0.2.1")),
        current_network: Some(context("192.0.2.1")),
        ..Default::default()
    };
    c.resolve_network = Arc::new(|_| Ok(Some(context("192.0.2.1"))));
    c.redial_tick(|_| {}).await;
    assert_eq!(c.missing, 1, "暂停重拨不等于停止监测");
    assert!(c.loss.is_some(), "会话消失必须被发现");
    assert_eq!(c.attempts, 0, "暂停状态下不能自动重拨");
}
/// 重连关闭时仍要监测；否则用户看不到会话已经掉了。
#[tokio::test]
async fn reconnect_disabled_still_monitors() {
    let mock = crate::tests::Mock::new("direct");
    let api = PortalClient::new(&mock.base).unwrap();
    api.login("test-user", "test-password").await.unwrap();
    let mut c = Controller {
        settings: Settings {
            server: mock.base.clone(),
            ..Default::default()
        },
        api: Some(api),
        selected: Some("mine".into()),
        paused: true,
        bound_network: Some(context("192.0.2.1")),
        current_network: Some(context("192.0.2.1")),
        ..Default::default()
    };
    c.resolve_network = Arc::new(|_| Ok(Some(context("192.0.2.1"))));
    c.redial_tick(|_| {}).await;
    assert_eq!(c.missing, 1);
    assert!(c.loss.is_some());
    assert_eq!(c.attempts, 0);
}
/// 疑似被挤后即使连续缺失三次，也不能自动重拨去抢别人的线。
#[tokio::test]
async fn eviction_hold_blocks_repeated_redial() {
    let mock = crate::tests::Mock::new("direct");
    mock.script_sessions(vec![crowded_elsewhere()
        .iter()
        .map(|row| crate::tests::session_json(&row.radacctid, "203.0.113.11"))
        .collect()]);
    let api = PortalClient::new(&mock.base).unwrap();
    api.login("test-user", "test-password").await.unwrap();
    let mut c = Controller {
        settings: Settings {
            reconnect_mode: ReconnectMode::NewSession,
            ..Default::default()
        },
        api: Some(api),
        credential: Some(Credential::new("test-user".into(), "test-password".into())),
        selected: Some("mine".into()),
        paused: false,
        bound_network: Some(context("192.0.2.1")),
        current_network: Some(context("192.0.2.1")),
        last_poll_failure: Some(Instant::now()),
        ..Default::default()
    };
    c.resolve_network = Arc::new(|_| Ok(Some(context("192.0.2.1"))));
    for _ in 0..5 {
        c.redial_tick(|_| {}).await;
    }
    assert!(c.redial_hold);
    assert_eq!(c.attempts, 0, "疑似被挤下线后不得自动抢线");
    assert!(
        !mock
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.contains("cmcc_login_value")),
        "不应重新走 Portal 认证"
    );
}
/// 同一告警只在状态变化时写一次日志，不随每轮轮询刷屏。
#[test]
fn suspected_eviction_is_logged_once() {
    let logs = crate::logging::LogBuffer::default();
    logs.set_enabled(true);
    let mut c = Controller {
        settings: Settings {
            log_enabled: true,
            ..Default::default()
        },
        logs: logs.clone(),
        rows: crowded_elsewhere(),
        selected: Some("mine".into()),
        current_network: Some(context("192.0.2.1")),
        last_poll_failure: Some(Instant::now()),
        ..Default::default()
    };
    for _ in 0..5 {
        c.update_session_status();
    }
    let hits = logs
        .entries()
        .iter()
        .filter(|entry| entry.code == "session.evicted_suspected")
        .count();
    assert_eq!(hits, 1, "同一目标丢失只该留下一条告警");
    assert!(
        logs.entries()
            .iter()
            .all(|entry| !entry.message.contains("已经确认")),
        "日志不能把推测写成事实"
    );
}
/// Memory + 未开重拨会立刻释放本地态，但告警文案与目标 ID 必须留下。
#[test]
fn memory_release_keeps_the_loss_notice() {
    let mut c = Controller {
        settings: Settings {
            credential_store: CredentialStore::Memory,
            reconnect_mode: ReconnectMode::Disabled,
            ..Default::default()
        },
        api: Some(PortalClient::new("http://127.0.0.1:9/lfradius/").unwrap()),
        credential: Some(Credential::new("test-user".into(), "test-password".into())),
        rows: crowded_elsewhere(),
        selected: Some("mine".into()),
        current_network: Some(context("192.0.2.1")),
        last_poll_failure: Some(Instant::now()),
        ..Default::default()
    };
    c.update_session_status();
    assert!(c.api.is_none() && c.selected.is_none(), "本地态应被释放");
    let snap = c.snapshot();
    assert!(
        snap.message.contains("疑似已被其他设备挤下线"),
        "{}",
        snap.message
    );
    assert_eq!(c.loss.as_ref().unwrap().id, "mine", "判断依据不能被清理掉");
}
/// 丢失后后台又冒出一条新会话：不能自动认领，它更可能是挤掉本机的设备。
#[test]
fn new_session_after_loss_is_not_auto_bound() {
    let mut c = Controller {
        rows: vec![session("someone-else", "203.0.113.11")],
        selected: None,
        awaiting_session: Some(HashSet::new()),
        loss: Some(SessionLoss {
            id: "mine".into(),
            kind: LossKind::Unclear,
        }),
        current_network: Some(context("192.0.2.1")),
        ..Default::default()
    };
    c.bind_new_session();
    assert!(c.selected.is_none(), "不能把别人的会话当成本机");
    assert!(c.awaiting_session.is_some(), "等待态交给用户显式处理");
}
/// 手动重新连接是唯一的恢复入口：旧告警与抢占暂停都要失效。
#[tokio::test]
async fn manual_reconnect_clears_the_eviction_hold() {
    let mock = crate::tests::Mock::new("direct");
    let mut c = Controller {
        settings: Settings {
            server: mock.base.clone(),
            reconnect_mode: ReconnectMode::NewSession,
            ..Default::default()
        },
        loss: Some(SessionLoss {
            id: "mine".into(),
            kind: LossKind::SuspectedEviction,
        }),
        redial_hold: true,
        ..Default::default()
    };
    c.resolve_network = Arc::new(|_| Ok(Some(context("127.0.0.1"))));
    c.connect(
        Credential::new("test-user".into(), "test-password".into()),
        "",
        false,
        |_| {},
    )
    .await
    .unwrap();
    assert!(c.loss.is_none(), "旧结论不能残留到新连接");
    assert!(!c.redial_hold, "手动重连后应恢复正常的重拨逻辑");
}
/// 选择会话只是换了管理目标，不等于本机已经安全上线。
///
/// 「疑似被挤下线」期间的自动重拨保护不能被一次普通 UI 选择解除：用户
/// 随手点中的那条很可能是挤掉本机的设备，恢复重拨会用本机账号再认证一次，
/// 反而可能把对方挤下去。要恢复只能显式重新连接。
#[test]
fn selecting_a_session_does_not_lift_the_eviction_hold() {
    let mut c = Controller {
        settings: Settings {
            reconnect_mode: ReconnectMode::NewSession,
            ..Default::default()
        },
        rows: crowded_elsewhere(),
        selected: Some("mine".into()),
        current_network: Some(context("192.0.2.1")),
        last_poll_failure: Some(Instant::now()),
        ..Default::default()
    };
    c.update_session_status();
    assert_eq!(
        c.loss.as_ref().map(|loss| loss.kind),
        Some(LossKind::SuspectedEviction),
        "前置条件：应当已判定为疑似被挤下线"
    );
    assert!(c.redial_hold);
    c.select("other-1").unwrap();
    assert!(c.loss.is_none(), "换目标后旧目标的告警应当消失");
    assert!(c.redial_hold, "选择会话不得解除疑似被挤下线期间的重拨保护");
}
/// 低频刷新也要判定，但不能替代 5 秒 worker 的缺失计数。
#[tokio::test]
async fn refresh_observes_without_advancing_missing() {
    let mock = crate::tests::Mock::new("direct");
    mock.script_sessions(vec![vec![
        crate::tests::session_json("other-1", "203.0.113.11"),
        crate::tests::session_json("other-2", "203.0.113.12"),
        crate::tests::session_json("other-3", "203.0.113.13"),
    ]]);
    let api = PortalClient::new(&mock.base).unwrap();
    api.login("test-user", "test-password").await.unwrap();
    let mut c = Controller {
        settings: Settings {
            server: mock.base.clone(),
            reconnect_mode: ReconnectMode::NewSession,
            ..Default::default()
        },
        api: Some(api),
        selected: Some("mine".into()),
        bound_network: Some(context("192.0.2.1")),
        current_network: Some(context("192.0.2.1")),
        ..Default::default()
    };
    c.resolve_network = Arc::new(|_| Ok(Some(context("192.0.2.1"))));
    c.refresh().await.unwrap();
    assert_eq!(c.missing, 0, "计数只由 5 秒 worker 推进");
    assert!(c.loss.is_some(), "刷新仍要更新丢失状态");
    assert_eq!(c.status, "offline");
}
/// 三次以上监测都不应把多条会话按顺序绑定给本机。
#[test]
fn multiple_new_sessions_are_never_bound_in_list_order() {
    let mut c = Controller {
        rows: vec![
            session("first", "203.0.113.11"),
            session("second", "203.0.113.12"),
            session("third", "203.0.113.13"),
        ],
        awaiting_session: Some(HashSet::new()),
        current_network: Some(context("203.0.113.11")),
        ..Default::default()
    };
    c.bind_new_session();
    assert!(c.selected.is_none(), "多条新增时必须让用户自己选");
}
fn portal_auth_requests(mock: &crate::tests::Mock) -> usize {
    mock.requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.contains("user_login") || request.contains("cmcc_login_value"))
        .count()
}
/// 保护不是"这一轮列表里还有选中会话"就能解除的。
///
/// 从原会话丢失 → 改选另一条在线会话 → 成功轮询若干次 → 新目标也消失，
/// 全程都不允许自动重拨，也不允许出现一次 Portal 认证请求。
#[tokio::test]
async fn eviction_hold_survives_polls_and_a_second_loss() {
    let mock = crate::tests::Mock::new("direct");
    let crowded = vec![
        crate::tests::session_json("other-1", "203.0.113.11"),
        crate::tests::session_json("other-2", "203.0.113.12"),
    ];
    mock.script_sessions(vec![
        crowded.clone(),
        crowded,
        vec![crate::tests::session_json("other-2", "203.0.113.12")],
    ]);
    let api = PortalClient::new(&mock.base).unwrap();
    api.login("test-user", "test-password").await.unwrap();
    let auth_before = portal_auth_requests(&mock);
    let mut c = Controller {
        settings: Settings {
            server: mock.base.clone(),
            reconnect_mode: ReconnectMode::NewSession,
            ..Default::default()
        },
        api: Some(api),
        credential: Some(Credential::new("test-user".into(), "test-password".into())),
        selected: Some("mine".into()),
        loss: Some(SessionLoss {
            id: "mine".into(),
            kind: LossKind::SuspectedEviction,
        }),
        redial_hold: true,
        paused: false,
        bound_network: Some(context("192.0.2.1")),
        current_network: Some(context("192.0.2.1")),
        ..Default::default()
    };
    c.resolve_network = Arc::new(|_| Ok(Some(context("192.0.2.1"))));
    c.redial_tick(|_| {}).await;
    assert!(c.redial_hold, "原会话仍缺失时保护不能被轮询清掉");
    c.select("other-1").unwrap();
    assert!(c.redial_hold, "切换会话不能解除保护");
    c.redial_tick(|_| {}).await;
    assert_eq!(c.status, "session_online");
    assert!(c.redial_hold, "成功轮询看到选中会话在线也不能解除保护");
    let snap = c.snapshot();
    assert!(snap.background_paused, "保护生效时后台必须保持暂停");
    assert!(
        snap.message.contains("自动重拨仍未恢复"),
        "提示必须说明保护仍在：{}",
        snap.message
    );
    for _ in 0..4 {
        c.redial_tick(|_| {}).await;
    }
    assert!(c.missing >= 3, "前置条件：新目标也应累计到重拨阈值");
    assert!(c.redial_hold);
    assert_eq!(c.attempts, 0, "保护期间不得自动重拨");
    assert!(c.credential.is_some(), "凭据不能被消费");
    assert!(c.api.is_some(), "监测不能停");
    assert_eq!(
        portal_auth_requests(&mock),
        auth_before,
        "保护期间不得发起任何 Portal 认证"
    );
}
/// 保护期间遇到真实传输层失败：监测继续，失败自愈不得重新认证。
#[tokio::test]
async fn eviction_hold_blocks_recovery_after_network_poll_failure() {
    let mock = crate::tests::Mock::new("direct");
    let mut c = Controller {
        settings: Settings {
            server: mock.base.clone(),
            reconnect_mode: ReconnectMode::NewSession,
            ..Default::default()
        },
        // 打不通的本地端口：模拟传输层失败，不依赖外部 Portal。
        api: Some(PortalClient::new("http://127.0.0.1:9/lfradius/").unwrap()),
        credential: Some(Credential::new("test-user".into(), "test-password".into())),
        selected: Some("mine".into()),
        loss: Some(SessionLoss {
            id: "mine".into(),
            kind: LossKind::SuspectedEviction,
        }),
        redial_hold: true,
        paused: false,
        bound_network: Some(context("192.0.2.1")),
        current_network: Some(context("192.0.2.1")),
        ..Default::default()
    };
    c.resolve_network = Arc::new(|_| Ok(Some(context("192.0.2.1"))));
    c.redial_tick(|_| {}).await;
    assert!(c.api.is_some(), "保护期间监测不能停");
    assert!(!c.paused, "保护期间不能把后台监测关掉");
    assert_eq!(c.attempts, 0, "失败自愈不得发起重拨");
    assert_eq!(c.network_attempts, 0, "失败自愈根本不应启动");
    assert!(c.credential.is_some(), "凭据不能被消费");
    assert!(c.redial_hold, "错误不应解除保护");
    assert!(
        c.last_poll_failure.is_some(),
        "传输层失败仍应作为本机连接异常的证据"
    );
    assert_eq!(portal_auth_requests(&mock), 0, "不得发起 Portal 认证");
    assert!(
        mock.requests.lock().unwrap().is_empty(),
        "保护期间不应向后台发送任何请求"
    );
}
/// 保护期间遇到后台拒绝（Cookie 失效）同样不能借自愈重新认证；
/// 这类错误也不能记成本机连接异常。
#[tokio::test]
async fn eviction_hold_blocks_recovery_after_rejected_poll() {
    let mock = crate::tests::Mock::new("direct");
    // 不登录：onlinelog 返回 success:0，客户端映射为 Rejected。
    let api = PortalClient::new(&mock.base).unwrap();
    let mut c = Controller {
        settings: Settings {
            server: mock.base.clone(),
            reconnect_mode: ReconnectMode::NewSession,
            ..Default::default()
        },
        api: Some(api),
        credential: Some(Credential::new("test-user".into(), "test-password".into())),
        selected: Some("mine".into()),
        loss: None,
        redial_hold: true,
        paused: false,
        bound_network: Some(context("192.0.2.1")),
        current_network: Some(context("192.0.2.1")),
        ..Default::default()
    };
    c.resolve_network = Arc::new(|_| Ok(Some(context("192.0.2.1"))));
    c.redial_tick(|_| {}).await;
    assert!(c.api.is_some(), "保护期间监测不能停");
    assert!(!c.paused, "保护期间不能把后台监测关掉");
    assert_eq!(c.attempts, 0);
    assert!(c.credential.is_some(), "凭据不能被消费");
    assert!(c.redial_hold);
    assert!(
        c.last_poll_failure.is_none(),
        "业务拒绝不能当成本机网络异常的证据"
    );
    assert_eq!(portal_auth_requests(&mock), 0, "不得发起 Portal 认证");
    assert!(
        mock.requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.contains("onlinelog")),
        "只允许继续查询会话"
    );
}
/// 本机地址判定必须是保守三态：未知不等于不匹配。
#[test]
fn local_ip_lookup_is_tri_state() {
    let c = Controller {
        current_network: Some(context("192.0.2.1")),
        ..Default::default()
    };
    let unknown = |id: &str| {
        let mut row = session(id, "203.0.113.99");
        row.framedipaddress = None;
        row
    };
    assert_eq!(c.local_ip_in(&[session("a", "192.0.2.1")]), Some(true));
    assert_eq!(c.local_ip_in(&crowded_elsewhere()), Some(false));
    assert_eq!(c.local_ip_in(&[unknown("a"), unknown("b")]), None);
    assert_eq!(
        c.local_ip_in(&[session("a", "203.0.113.11"), unknown("b")]),
        None,
        "存在未知地址时不能断言本机不在列表"
    );
    assert_eq!(
        c.local_ip_in(&[unknown("a"), session("b", "192.0.2.1")]),
        Some(true),
        "命中优先于未知"
    );
    assert_eq!(c.local_ip_in(&[]), Some(false), "空列表没有任何未知项");
}
/// 部分会话缺地址时，「本机 IP 不在列表」不成立，只能降级成原因不明。
#[test]
fn partial_unknown_addresses_downgrade_eviction_to_unclear() {
    let mut c = observing(
        vec![
            session("other-1", "203.0.113.11"),
            session("other-2", "203.0.113.12"),
            {
                let mut row = session("other-3", "203.0.113.13");
                row.framedipaddress = None;
                row
            },
        ],
        "mine",
    );
    c.settings.reconnect_mode = ReconnectMode::NewSession;
    c.last_poll_failure = Some(Instant::now());
    c.update_session_status();
    assert_eq!(
        c.loss.as_ref().map(|loss| loss.kind),
        Some(LossKind::Unclear),
        "证据不完整时不能判成疑似被挤下线"
    );
    assert!(!c.redial_hold, "原因不明时不应停掉自动重拨");
}
/// 只有传输层失败/超时能当作本机连接异常；后台、登录态、解析错误都不算。
#[test]
fn only_transport_failures_are_connection_evidence() {
    assert!(Controller::poll_failure_is_network(&AppError::Network(
        crate::NetworkFault::Connect
    )));
    assert!(Controller::poll_failure_is_network(&AppError::Network(
        crate::NetworkFault::Timeout
    )));
    assert!(Controller::poll_failure_is_network(&AppError::Timeout(
        "下线确认"
    )));
    for error in [
        AppError::Http(500),
        AppError::Http(503),
        AppError::Rejected,
        AppError::InvalidResponse("后台会话列表缺少字段"),
        AppError::Redirect,
    ] {
        assert!(
            !Controller::poll_failure_is_network(&error),
            "{error:?} 不能当作本机连接异常"
        );
    }
}
/// 成功轮询后，之前的失败证据要清掉，不能继续留着升级下一次判定。
#[tokio::test]
async fn successful_poll_clears_the_failure_evidence() {
    let mock = crate::tests::Mock::new("direct");
    mock.script_sessions(vec![vec![crate::tests::session_json("mine", "192.0.2.1")]]);
    let api = PortalClient::new(&mock.base).unwrap();
    api.login("test-user", "test-password").await.unwrap();
    let mut c = Controller {
        settings: Settings {
            server: mock.base.clone(),
            reconnect_mode: ReconnectMode::NewSession,
            ..Default::default()
        },
        api: Some(api),
        selected: Some("mine".into()),
        last_poll_failure: Some(Instant::now()),
        paused: false,
        bound_network: Some(context("192.0.2.1")),
        current_network: Some(context("192.0.2.1")),
        ..Default::default()
    };
    c.resolve_network = Arc::new(|_| Ok(Some(context("192.0.2.1"))));
    c.redial_tick(|_| {}).await;
    assert!(c.last_poll_failure.is_none(), "成功轮询后旧证据必须失效");
    assert_eq!(c.missing, 0);
    assert_eq!(c.status, "session_online");
}
fn account(plan: &str) -> AccountInfo {
    AccountInfo {
        plan: plan.into(),
        bandwidth: None,
        expires_on: None,
        unpurchased: false,
    }
}
/// 快照只回滚“连接相关”的状态；失败证据、重试计数、凭据与 `reconnecting`
/// 必须保持调用方在 `connect()` 返回后写入的新值。
#[test]
fn reconnect_checkpoint_restores_connection_state_and_keeps_new_evidence() {
    let mut c = Controller {
        api: Some(PortalClient::new("http://127.0.0.1:9/lfradius/").unwrap()),
        credential: Some(Credential::new("old-user".into(), "old-pass".into())),
        account: Some(account("旧套餐")),
        rows: vec![row("mine", 60, 1, 2)],
        selected: Some("mine".into()),
        awaiting_session: Some(["mine".to_owned()].into_iter().collect()),
        latest_rates: HashMap::from([(
            "mine".to_owned(),
            Rate {
                upload_bps: Some(1.0),
                download_bps: Some(2.0),
                sample_seconds: Some(5),
            },
        )]),
        paused: false,
        status: "session_online".into(),
        message: "旧提示".into(),
        missing: 2,
        loss: Some(SessionLoss {
            id: "mine".into(),
            kind: LossKind::Unclear,
        }),
        redial_hold: true,
        bound_network: Some(context("192.0.2.1")),
        current_network: Some(context("192.0.2.1")),
        last_poll_failure: Some(Instant::now()),
        attempts: 2,
        network_attempts: 3,
        ..Default::default()
    };
    let checkpoint = ReconnectCheckpoint::capture(&c);
    let evidence = Instant::now();
    c.api = None;
    c.account = Some(account("被丢弃的新套餐"));
    c.rows.clear();
    c.selected = Some("other".into());
    c.awaiting_session = None;
    c.latest_rates.clear();
    c.paused = true;
    c.status = "error".into();
    c.message = "新提示".into();
    c.missing = 9;
    c.loss = None;
    c.redial_hold = false;
    c.bound_network = Some(context("192.0.2.9"));
    // 以下属于“失败后必须保留的新证据/新决策”，快照不应覆盖。
    c.current_network = Some(context("192.0.2.9"));
    c.last_poll_failure = Some(evidence);
    c.attempts = 0;
    c.network_attempts = 4;
    c.credential = None;
    c.reconnecting = true;
    checkpoint.restore(&mut c);
    assert!(c.api.is_some(), "旧 client 必须退回");
    assert_eq!(c.account.as_ref().map(|a| a.plan.as_str()), Some("旧套餐"));
    assert_eq!(c.rows.len(), 1);
    assert_eq!(c.selected.as_deref(), Some("mine"));
    assert!(c.awaiting_session.is_some());
    assert_eq!(c.latest_rates.len(), 1);
    assert!(!c.paused);
    assert_eq!(c.status, "session_online");
    assert_eq!(c.message, "旧提示");
    assert_eq!(c.missing, 2);
    assert!(c.loss.is_some());
    assert!(c.redial_hold, "全量回滚不能顺手解除保护");
    assert_eq!(
        c.bound_network.as_ref().map(|n| n.ipv4.as_str()),
        Some("192.0.2.1")
    );
    // 失败证据与调度字段保持“新值”。
    assert_eq!(
        c.current_network.as_ref().map(|n| n.ipv4.as_str()),
        Some("192.0.2.9"),
        "新解析到的网卡地址不能回滚"
    );
    assert_eq!(c.last_poll_failure, Some(evidence));
    assert_eq!(c.attempts, 0);
    assert_eq!(c.network_attempts, 4);
    assert!(c.credential.is_none());
    assert!(c.reconnecting);
}
/// 认证被拒后的部分回滚：旧连接相关字段退回，`paused`/`status`/`message`
/// 与新的 `loss` 结论保留，不能被旧快照盖掉。
#[test]
fn reconnect_checkpoint_connection_restore_keeps_the_rejected_decision() {
    let mut c = Controller {
        api: Some(PortalClient::new("http://127.0.0.1:9/lfradius/").unwrap()),
        account: Some(account("旧套餐")),
        rows: vec![row("mine", 60, 0, 0)],
        selected: Some("mine".into()),
        bound_network: Some(context("192.0.2.1")),
        loss: Some(SessionLoss {
            id: "mine".into(),
            kind: LossKind::SuspectedEviction,
        }),
        ..Default::default()
    };
    let checkpoint = ReconnectCheckpoint::capture(&c);
    c.api = None;
    c.account = None;
    c.rows.clear();
    c.selected = Some("other".into());
    c.bound_network = Some(context("192.0.2.9"));
    c.status = "error".into();
    c.message = "认证被拒".into();
    c.paused = true;
    c.loss = None;
    checkpoint.restore_connection(&mut c);
    assert!(c.api.is_some(), "新连接已被丢弃，旧 client 必须退回");
    assert_eq!(c.account.as_ref().map(|a| a.plan.as_str()), Some("旧套餐"));
    assert_eq!(c.selected.as_deref(), Some("mine"));
    assert_eq!(
        c.bound_network.as_ref().map(|n| n.ipv4.as_str()),
        Some("192.0.2.1"),
        "绑定地址不能指向被丢弃的新连接"
    );
    assert_eq!(c.status, "error");
    assert_eq!(c.message, "认证被拒");
    assert!(c.paused);
    assert!(c.loss.is_none(), "新的失败结论不能被旧快照恢复");
}
/// 保护期间根本不会拍快照（准入在 `capture` 之前）；即便拍到 `true`，
/// 恢复也不能把它改成 `false`。
#[test]
fn reconnect_checkpoint_never_lifts_the_protection() {
    let mut c = Controller {
        selected: Some("mine".into()),
        redial_hold: true,
        ..Default::default()
    };
    let checkpoint = ReconnectCheckpoint::capture(&c);
    c.redial_hold = false;
    c.selected = Some("other".into());
    checkpoint.restore(&mut c);
    assert!(c.redial_hold);
    assert_eq!(c.selected.as_deref(), Some("mine"));
}
/// `TerminateAndReconnect` 只在本地 `clear()`，不会终止服务端会话；网络
/// 失败时旧 client、会话、账号必须整体退回，且不得重新认证。
#[tokio::test]
async fn redial_tick_rolls_back_a_failed_redial_after_local_clear() {
    let mock = crate::tests::Mock::new("direct");
    mock.script_sessions(vec![vec![crate::tests::session_json(
        "other",
        "203.0.113.11",
    )]]);
    let api = PortalClient::new(&mock.base).unwrap();
    api.login("test-user", "test-password").await.unwrap();
    let mut c = Controller {
        settings: Settings {
            // 打不通的本地端口：connect 必然以传输层错误失败。
            server: "http://127.0.0.1:9/lfradius/".into(),
            reconnect_mode: ReconnectMode::TerminateAndReconnect,
            ..Default::default()
        },
        api: Some(api),
        credential: Some(Credential::new("test-user".into(), "test-password".into())),
        account: Some(account("旧套餐")),
        selected: Some("mine".into()),
        missing: 2,
        paused: false,
        bound_network: Some(context("192.0.2.1")),
        current_network: Some(context("192.0.2.1")),
        ..Default::default()
    };
    c.resolve_network = Arc::new(|_| Ok(Some(context("192.0.2.1"))));
    c.redial_tick(|_| {}).await;
    assert_eq!(c.attempts, 0, "瞬时失败要清零重试计数");
    assert!(c.credential.is_some(), "凭据必须退回");
    assert!(c.api.is_some(), "旧后台 client 必须退回");
    assert_eq!(
        c.account.as_ref().map(|a| a.plan.as_str()),
        Some("旧套餐"),
        "账号信息不能被 clear 永久丢掉"
    );
    assert_eq!(c.selected.as_deref(), Some("mine"), "旧选中目标必须退回");
    assert_eq!(c.status, "offline", "轮询得出的会话状态要保留");
    assert!(
        c.loss.is_some(),
        "快照拍在轮询之后，这一轮得到的丢失结论不能被回滚抹掉"
    );
    assert!(
        c.message.contains("网络暂时不可用"),
        "回滚后的提示要说明失败原因：{}",
        c.message
    );
    assert_eq!(portal_auth_requests(&mock), 1, "只允许测试夹具的登录");
    assert!(
        mock.requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| !request.contains("cmcc_login_value")),
        "失败的重拨不得发起 Portal 认证"
    );
}
/// 网卡解析失败属于环境问题，不能落进"认证被拒"分支：本地 clear 后必须
/// 按快照整体退回，不得永久暂停，也不得发出 Portal 认证请求。
#[tokio::test]
async fn redial_tick_treats_interface_failure_as_retryable() {
    let mock = crate::tests::Mock::new("direct");
    mock.script_sessions(vec![vec![crate::tests::session_json(
        "other",
        "203.0.113.11",
    )]]);
    let api = PortalClient::new(&mock.base).unwrap();
    api.login("test-user", "test-password").await.unwrap();
    let mut c = Controller {
        settings: Settings {
            // 与快照里的旧 client 同源：任何意外发出的重连请求都会记在
            // 这个 Mock 上，便于断言"没有发起认证"。
            server: mock.base.clone(),
            reconnect_mode: ReconnectMode::TerminateAndReconnect,
            probe_enabled: false,
            ..Default::default()
        },
        api: Some(api),
        credential: Some(Credential::new("test-user".into(), "test-password".into())),
        account: Some(account("旧套餐")),
        selected: Some("mine".into()),
        missing: 2,
        paused: false,
        bound_network: Some(context("192.0.2.1")),
        current_network: Some(context("192.0.2.1")),
        ..Default::default()
    };
    c.resolve_network = Arc::new(|_| Err("没有找到带 IPv4 和 MAC 的活动网卡".into()));
    c.redial_tick(|_| {}).await;
    assert!(!c.paused, "网卡暂时不可用不能暂停自动重拨");
    assert_eq!(c.attempts, 0, "可重试失败要清零连续失败计数");
    assert!(c.api.is_some(), "本地 clear 后必须退回旧后台 client");
    assert_eq!(c.selected.as_deref(), Some("mine"), "旧选中目标必须退回");
    assert_eq!(c.rows.len(), 1, "旧会话列表必须退回");
    assert_eq!(
        c.account.as_ref().map(|a| a.plan.as_str()),
        Some("旧套餐"),
        "账号信息不能被 clear 永久丢掉"
    );
    assert_eq!(c.status, "offline", "不能留下 connect() 的 error 状态");
    assert!(
        c.message.contains("网络暂时不可用"),
        "应说明是环境问题而不是认证被拒：{}",
        c.message
    );
    assert!(
        !c.message.contains("认证系统拒绝"),
        "网卡解析失败被误报成认证拒绝：{}",
        c.message
    );
    assert_eq!(portal_auth_requests(&mock), 1, "只允许测试夹具的登录");
    assert!(
        mock.requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| !request.contains("cmcc_login_value")),
        "解析不到网卡时不得发起 Portal 认证"
    );
}
/// 成功绑定会话必须结束这一轮连续失败计数：否则三次成功后 `attempts`
/// 停在 3，下一次掉线会被 `attempts >= 3` 永久挡住。
#[tokio::test]
async fn successful_redial_resets_attempts_for_the_next_drop() {
    let mock = crate::tests::Mock::new("direct");
    // 解析出的源地址必须是本机真实可绑定的地址，`connect()` 才能在
    // 认证后用同一地址认领这条会话。
    mock.script_sessions(vec![vec![crate::tests::session_json("mine", "127.0.0.1")]]);
    let api = PortalClient::new(&mock.base).unwrap();
    api.login("test-user", "test-password").await.unwrap();
    let mut c = Controller {
        settings: Settings {
            server: mock.base.clone(),
            reconnect_mode: ReconnectMode::NewSession,
            probe_enabled: false,
            ..Default::default()
        },
        api: Some(api),
        credential: Some(Credential::new("test-user".into(), "test-password".into())),
        selected: Some("gone".into()),
        missing: 3,
        paused: false,
        bound_network: Some(context("127.0.0.1")),
        current_network: Some(context("127.0.0.1")),
        ..Default::default()
    };
    c.resolve_network = Arc::new(|_| Ok(Some(context("127.0.0.1"))));
    // 三轮"旧会话消失 → 重拨并按本机地址绑定"：旧实现每次成功都保留
    // 计数，第三次成功后 `attempts` 停在 3。
    for round in 1..=3 {
        c.selected = Some("gone".into());
        c.missing = 3;
        c.last_attempt = None;
        c.redial_tick(|_| {}).await;
        assert_eq!(
            c.selected.as_deref(),
            Some("mine"),
            "第 {round} 轮应绑定到本机对应的新会话"
        );
        assert_eq!(c.attempts, 0, "第 {round} 轮成功后连续失败计数必须归零");
    }
    assert!(!c.paused);

    // 第四次掉线换成一个打不通的后台：必须真的发起重拨并走可重试分支，
    // 而不是在 `attempts >= 3` 处静默返回。
    c.settings.server = "http://127.0.0.1:9/lfradius/".into();
    c.selected = Some("gone".into());
    c.missing = 3;
    c.last_attempt = None;
    c.redial_tick(|_| {}).await;
    assert_eq!(c.attempts, 0, "瞬时失败同样归零");
    assert!(c.credential.is_some(), "凭据必须退回");
    assert!(!c.paused, "瞬时失败不暂停");
    assert!(
        c.message.contains("网络暂时不可用"),
        "新的失败序列必须真的发起重拨：{}",
        c.message
    );
}
/// 网络自愈失败后的回滚不能抹掉刚发生的轮询失败证据，也不能吞掉旧会话。
#[tokio::test]
async fn retry_after_failure_restores_state_and_keeps_the_poll_failure_evidence() {
    let mut c = Controller {
        settings: Settings {
            server: "http://127.0.0.1:9/lfradius/".into(),
            reconnect_mode: ReconnectMode::NewSession,
            ..Default::default()
        },
        api: Some(PortalClient::new("http://127.0.0.1:9/lfradius/").unwrap()),
        credential: Some(Credential::new("test-user".into(), "test-password".into())),
        account: Some(account("旧套餐")),
        rows: vec![row("mine", 60, 0, 0)],
        selected: Some("mine".into()),
        paused: false,
        bound_network: Some(context("192.0.2.1")),
        current_network: Some(context("192.0.2.1")),
        ..Default::default()
    };
    c.resolve_network = Arc::new(|_| Ok(Some(context("192.0.2.1"))));
    c.redial_tick(|_| {}).await;
    assert_eq!(c.network_attempts, 1, "可重试失败要记一次自愈尝试");
    assert!(
        c.last_poll_failure.is_some(),
        "刚发生的传输层失败是证据，回滚不能抹掉"
    );
    assert!(c.api.is_some());
    assert_eq!(c.rows.len(), 1, "旧会话列表必须退回");
    assert_eq!(c.selected.as_deref(), Some("mine"));
    assert_eq!(c.account.as_ref().map(|a| a.plan.as_str()), Some("旧套餐"));
    assert!(c.credential.is_some());
    assert!(!c.paused);
    assert!(
        c.message.contains("正在按退避自动重连"),
        "失败提示不能被回滚成旧文案：{}",
        c.message
    );
}
