//! [`LiveSignin`]: the [`SigninApi`] the app hands both frontends. Each
//! flow runs on the background runtime (`familychat-session`'s
//! `BackgroundRuntime`), so nothing a frontend calls blocks on the
//! network.

use std::sync::Arc;

use familychat_core::signin::{SigninApi, SigninFlow, SigninStart, SigninStep};
use familychat_session::AccountManager;
use tokio::{
    runtime::Handle,
    sync::{Mutex, mpsc, watch},
};

use crate::{
    allowlist::Allowlist,
    device::device_display_name,
    flow::{Services, run_flow},
    http::Transport,
};

/// Starts sign-in flows on the background runtime.
pub struct LiveSignin {
    services: Arc<Services>,
    handle: Handle,
}

impl std::fmt::Debug for LiveSignin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveSignin")
            .field("allowlist", &self.services.allowlist)
            .finish_non_exhaustive()
    }
}

impl LiveSignin {
    /// The app's sign-in: new accounts go to `manager`, requests through
    /// `transport` (`ReqwestTransport::new()`), under this build's
    /// allowlist, with this OS's device name.
    pub fn new(
        manager: Arc<AccountManager>,
        transport: Arc<dyn Transport>,
        handle: Handle,
    ) -> Self {
        Self::with_services(
            Services {
                transport,
                accounts: manager,
                allowlist: Allowlist::for_build(),
                gate: Arc::new(Mutex::new(())),
                device_name: device_display_name().to_owned(),
            },
            handle,
        )
    }

    /// A sign-in over explicit services (tests).
    pub fn with_services(services: Services, handle: Handle) -> Self {
        Self {
            services: Arc::new(services),
            handle,
        }
    }
}

impl SigninApi for LiveSignin {
    fn start(&self, start: SigninStart) -> SigninFlow {
        let (steps, receiver) = watch::channel(SigninStep::Starting);
        let (answers, answers_receiver) = mpsc::unbounded_channel();
        self.handle.spawn(run_flow(
            self.services.clone(),
            start,
            steps,
            answers_receiver,
        ));
        SigninFlow::new(receiver, answers)
    }
}
