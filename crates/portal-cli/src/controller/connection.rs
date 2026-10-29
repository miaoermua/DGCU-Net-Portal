//! 连接生命周期：一次完整的认证事务与用户显式下线。
//!
//! `connect` 同时承载自动重拨的事务性语义（`NewSession` 不提前丢掉旧连接），
//! `disconnect` 是用户主动下线。网卡重绑定与会话认领是 `Controller` 上的
//! 共享原语，由轮询和重拨自行调用。
use super::{Controller, Snapshot};
use crate::{
    cmcc::Phase,
    settings::{CredentialStore, ReconnectMode},
    AppError, CmccContext, Credential, PortalClient, PortalLoginOutcome,
};
use std::net::IpAddr;

impl Controller {
    pub async fn connect<F: Fn(Phase)>(
        &mut self,
        credential: Credential,
        portal_url: &str,
        backend_only: bool,
        progress: F,
    ) -> Result<Snapshot, AppError> {
        // A manual connection starts a fresh local session. Automatic redial is
        // transactional: keep the current API/Cookie and session rows until the
        // replacement connection has completed successfully.
        if !self.reconnecting {
            self.clear();
        }
        let resolved = (self.resolve_network)(&self.settings.interface_name);
        let network = if backend_only {
            resolved.ok().flatten()
        } else {
            Some(
                resolved
                    .map_err(AppError::NetworkInterface)
                    .and_then(|value| {
                        value.ok_or_else(|| {
                            AppError::NetworkInterface(
                                "没有找到带 IPv4 和 MAC 的活动网卡，请在设置中选择网卡".into(),
                            )
                        })
                    })
                    .map_err(|e| self.fail(e))?,
            )
        };
        let local_address = network.as_ref().map(|value| {
            value
                .ipv4
                .parse::<IpAddr>()
                .map_err(|_| self.fail(AppError::NetworkInterface("网卡 IPv4 地址无效".into())))
        });
        let local_address = match local_address {
            Some(Ok(value)) => Some(value),
            Some(Err(error)) => return Err(error),
            None => None,
        };
        let api = PortalClient::with_options_and_local(
            &self.settings.server,
            self.settings.bypass_proxy,
            local_address,
        )?
        .with_logs(self.logs.clone());
        let portal = PortalClient::with_options_and_local(
            &self.settings.server,
            self.settings.bypass_proxy,
            local_address,
        )?
        .with_logs(self.logs.clone());
        // 记下这次 client 绑定的网卡上下文；后续轮询据此判断是否需要重建。
        self.bound_network = network.clone();
        self.status = "authenticating".into();
        // Authenticate the self-service client before Portal login so we can record a
        // baseline. The same cookie jar is reused after Portal login; a second
        // user_login request is only needed when this first attempt failed.
        let mut backend_authenticated = false;
        let baseline = match api.login(&credential.username, &credential.password).await {
            Ok(()) => {
                backend_authenticated = true;
                // 套餐与到期时间只在连接时读一次：它随订购变更，不随会话变化。
                self.account = api.account().await.ok();
                api.sessions().await.ok()
            }
            Err(e) => {
                self.logs.error(&e);
                None
            }
        };
        if let Some(rows) = baseline.as_ref() {
            if let Some(id) = Self::unique_local_session(rows, network.as_ref()) {
                self.api = Some(api);
                self.rows = rows.clone();
                self.selected = Some(id);
                self.awaiting_session = None;
                self.missing = 0;
                self.status = "session_online".into();
                self.message = "已恢复本机对应的在线会话".into();
                // 自动重拨走的也是 connect，但不会先 `clear()`；这里是成功恢复
                // 的分支，旧的"会话丢失"结论必须失效，否则 `snapshot()` 会继续
                // 把旧告警压在"已恢复"之上。抢占保护则只由显式恢复解除：
                // 自动路径在保护期间根本不会走到这里。
                self.loss = None;
                if !self.reconnecting {
                    self.lift_eviction_protection();
                }
                self.paused = self.settings.reconnect_mode == ReconnectMode::Disabled;
                // 凭据只存在控制器内存里；只要开了重连就保留，与密码是否落盘无关。
                if self.settings.reconnect_mode != ReconnectMode::Disabled {
                    self.credential = Some(credential);
                }
                return Ok(self.snapshot());
            }
        }
        let outcome = if backend_only {
            None
        } else {
            let ctx = if portal_url.is_empty() {
                progress(Phase::ReadingForm);
                // The DGCU gateway accepts the standard CMCC entry with the
                // current interface context. This avoids depending on public
                // captive-check hosts, which often time out before login.
                let template = CmccContext::from_server_context_options(
                    &self.settings.server,
                    network.as_ref().unwrap(),
                    &self.settings.paip,
                    (!self.settings.basip.is_empty()).then_some(self.settings.basip.as_str()),
                )?;
                if self.settings.probe_enabled {
                    match portal.portal_form_available(&template).await {
                        Ok(()) => Ok(template),
                        Err(_) => {
                            progress(Phase::Discovering);
                            let discovered = portal.discover(&self.settings.probe_url).await?;
                            CmccContext::with_network_context_options(
                                &discovered.portal_url,
                                network.as_ref().unwrap(),
                                &self.settings.paip,
                                (!self.settings.basip.is_empty())
                                    .then_some(self.settings.basip.as_str()),
                            )
                        }
                    }
                } else {
                    Ok(template)
                }
            } else {
                progress(Phase::ReadingForm);
                CmccContext::from_portal_url(portal_url).and_then(|ctx| {
                    CmccContext::with_network_context_options(
                        &ctx.portal_url,
                        network.as_ref().unwrap(),
                        &self.settings.paip,
                        (!self.settings.basip.is_empty()).then_some(self.settings.basip.as_str()),
                    )
                })
            };
            let ctx = ctx.map_err(|e| self.fail(e))?;
            Some(
                portal
                    .cmcc_login_progress(&ctx, &credential.username, &credential.password, progress)
                    .await
                    .map_err(|e| self.fail(e))?,
            )
        };
        // Portal cookies do not authorize the separate user self-service endpoints.
        // If the baseline login succeeded, retain that authenticated API client
        // instead of issuing a duplicate login request.
        if !backend_authenticated {
            match api.login(&credential.username, &credential.password).await {
                Ok(()) => backend_authenticated = true,
                Err(e) if outcome.is_some() => {
                    self.status = "accepted".into();
                    self.message =
                        "Portal 已确认成功，但后台登录失败；请重新登录后台查询会话".into();
                    self.logs.error(&e);
                    return Ok(self.snapshot());
                }
                Err(e) => return Err(self.fail(e)),
            }
        }
        if backend_authenticated {
            if self.account.is_none() {
                self.account = api.account().await.ok();
            }
            self.api = Some(api);
        }
        let rows = self.api.as_ref().unwrap().sessions().await;
        let query_failed = rows.is_err();
        self.rows = match rows {
            Ok(rows) => rows,
            Err(error) => {
                self.logs.error(&error);
                Vec::new()
            }
        };
        let restored_local = Self::unique_local_session(&self.rows, network.as_ref());
        if let Some(id) = restored_local {
            self.selected = Some(id);
            self.awaiting_session = None;
            self.missing = 0;
        }
        // Retain the pre-login baseline until accounting reports the new session.
        if outcome.is_some() {
            self.awaiting_session =
                baseline.map(|rows| rows.iter().map(|row| row.radacctid.clone()).collect());
            self.bind_new_session();
        }
        self.status = if self.selected.is_some() && outcome.is_none() {
            "session_online"
        } else if outcome.is_some() {
            "accepted"
        } else {
            "backend"
        }
        .into();
        // 认证成功和"绑定到哪一条会话"是两件独立的事：多会话时 bind_new_session
        // 会放弃自动绑定以免挑错，但认证本身仍然成功。此时提示必须同时说清这两点，
        // 否则用户看到"已确认成功"、流量却是空的，没有任何线索知道要自己选。
        let bound = self.selected.is_some();
        self.message = match outcome {
            Some(PortalLoginOutcome::Dialed) if bound => "Portal 与代拨均已确认成功",
            Some(PortalLoginOutcome::Dialed) => {
                "Portal 与代拨均已确认成功，但尚未绑定管理会话；请在会话列表中选择"
            }
            Some(_) if bound => "Portal 已确认成功",
            Some(_) => "Portal 已确认成功，但尚未绑定管理会话；请在会话列表中选择",
            None if bound => "已登录后台并恢复本机对应的在线会话",
            None => "已登录用户后台；请选择要管理的会话",
        }
        .into();
        if query_failed {
            self.message
                .push_str("；后台会话查询暂不可用，稍后自动重试");
        }
        self.paused = self.settings.reconnect_mode == ReconnectMode::Disabled || outcome.is_none();
        // 连接成功是新开始：旧的"会话已消失"事实结论失效，不再压住后续提示。
        self.loss = None;
        // 抢占保护仍只由用户显式恢复解除。自动恢复（`reconnecting`）成功只说明
        // 后台这次登录通过，不足以证明本机不会再被挤，不能顺手放开自动重拨。
        if !self.reconnecting {
            self.lift_eviction_protection();
        }
        if self.settings.reconnect_mode != ReconnectMode::Disabled {
            self.credential = Some(credential);
        }
        // Otherwise credential drops here, before the online session ends.
        Ok(self.snapshot())
    }

    pub async fn disconnect(&mut self, id: &str) -> Result<Snapshot, AppError> {
        self.paused = true;
        self.loss = None;
        // 用户显式下线：本地已不再持有会话，保护没有继续压着的理由。
        self.lift_eviction_protection();
        // 手动下线是显式释放路径：重连凭据在这里必须作废，任何保存方式都一样。
        self.credential = None;
        let result = match &self.api {
            Some(api) => api.disconnect_id(id).await,
            None => Err(AppError::Rejected),
        };
        if let Err(e) = result {
            self.logs.error(&e);
            if self.settings.credential_store == CredentialStore::Memory {
                self.clear();
            }
            self.status = "unknown".into();
            self.message = "远端下线未确认；请在认证后台检查。本地重连凭据已释放".into();
            return Err(e);
        }
        self.rows.retain(|r| r.radacctid != id);
        if self.selected.as_deref() == Some(id) {
            self.selected = None;
        }
        if self.settings.credential_store == CredentialStore::Memory {
            self.clear();
        }
        self.status = "offline".into();
        self.message = "目标会话已确认下线".into();
        Ok(self.snapshot())
    }
}
