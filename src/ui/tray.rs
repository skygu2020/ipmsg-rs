use async_channel::Sender;
use ksni::blocking::TrayMethods;
use ksni::menu::{MenuItem, StandardItem};

#[derive(Clone, Copy, Debug)]
pub enum TrayAction {
    ShowMainWindow,
    Quit,
}

pub fn start(actions: Sender<TrayAction>) -> Sender<()> {
    let (shutdown_tx, shutdown_rx) = async_channel::bounded::<()>(1);

    std::thread::Builder::new()
        .name("ipmsg-system-tray".to_owned())
        .spawn(move || {
            let tray = IpmsgTray { actions };
            let handle = match tray.spawn() {
                Ok(handle) => handle,
                Err(error) => {
                    log::warn!("无法启动系统托盘: {error}");
                    return;
                }
            };

            // Keep the service handle alive until the GTK application shuts down.
            if shutdown_rx.recv_blocking().is_ok() {
                handle.shutdown().wait();
            }
        })
        .expect("failed to start system tray thread");

    shutdown_tx
}

struct IpmsgTray {
    actions: Sender<TrayAction>,
}

impl ksni::Tray for IpmsgTray {
    fn id(&self) -> String {
        "ipmsg-rs".to_owned()
    }

    fn title(&self) -> String {
        "飞鸽传书".to_owned()
    }

    fn icon_name(&self) -> String {
        "mail-unread".to_owned()
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.actions.try_send(TrayAction::ShowMainWindow);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let show_actions = self.actions.clone();
        let quit_actions = self.actions.clone();

        vec![
            StandardItem {
                label: "打开主窗口".to_owned(),
                icon_name: "window".to_owned(),
                activate: Box::new(move |_| {
                    let _ = show_actions.try_send(TrayAction::ShowMainWindow);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "退出".to_owned(),
                icon_name: "application-exit".to_owned(),
                activate: Box::new(move |_| {
                    let _ = quit_actions.try_send(TrayAction::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}
