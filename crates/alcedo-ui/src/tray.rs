//! Linux system tray via StatusNotifierItem (ksni).

#[cfg(target_os = "linux")]
mod imp {
    use alcedo_i18n::{ntr, trs};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    use ksni::blocking::TrayMethods as _;
    use ksni::menu::StandardItem;
    use ksni::{OfflineReason, Tray};
    use relm4::gtk::glib;

    use crate::settings;

    fn tray_icon_pixmaps() -> Vec<ksni::Icon> {
        crate::theme::app_icon_argb_pixmaps()
            .into_iter()
            .map(|icon| ksni::Icon {
                width: icon.width,
                height: icon.height,
                data: icon.argb32,
            })
            .collect()
    }

    fn marshal_to_main_loop(callback: Arc<dyn Fn() + Send + Sync>) {
        glib::MainContext::default().invoke(move || {
            callback();
        });
    }

    struct AlcedoTray {
        show: Arc<dyn Fn() + Send + Sync>,
        quit: Arc<dyn Fn() + Send + Sync>,
        icon_name: String,
        unread_count: AtomicU32,
        warned_watcher_offline: AtomicBool,
    }

    impl AlcedoTray {
        fn new(
            show: Arc<dyn Fn() + Send + Sync>,
            quit: Arc<dyn Fn() + Send + Sync>,
            icon_name: String,
        ) -> Self {
            Self {
                show,
                quit,
                icon_name,
                unread_count: AtomicU32::new(0),
                warned_watcher_offline: AtomicBool::new(false),
            }
        }

        fn show_on_main_loop(&self) {
            marshal_to_main_loop(self.show.clone());
        }
    }

    impl Tray for AlcedoTray {
        fn id(&self) -> String {
            "dev.alcedo.Mail".into()
        }

        fn title(&self) -> String {
            let unread = self.unread_count.load(Ordering::Relaxed);
            if unread > 0 {
                // TRANSLATORS: tray icon title.
                ntr!("Alcedo ({n} unread)", "Alcedo ({n} unread)", unread)
            } else {
                "Alcedo".into()
            }
        }

        fn icon_name(&self) -> String {
            self.icon_name.clone()
        }

        fn icon_theme_path(&self) -> String {
            crate::theme::tray_icon_theme_path()
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            tray_icon_pixmaps()
        }

        fn overlay_icon_name(&self) -> String {
            if self.unread_count.load(Ordering::Relaxed) > 0 {
                "mail-unread-symbolic".into()
            } else {
                String::new()
            }
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            self.show_on_main_loop();
        }

        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            let show = self.show.clone();
            let quit = self.quit.clone();
            vec![
                StandardItem {
                    label: trs!("Show Alcedo").into(),
                    activate: Box::new(move |_| {
                        marshal_to_main_loop(show.clone());
                    }),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: trs!("Quit").into(),
                    activate: Box::new(move |_| {
                        marshal_to_main_loop(quit.clone());
                    }),
                    ..Default::default()
                }
                .into(),
            ]
        }

        fn watcher_offline(&self, reason: OfflineReason) -> bool {
            if !self.warned_watcher_offline.swap(true, Ordering::Relaxed) {
                tracing::info!(
                    ?reason,
                    "StatusNotifier watcher unavailable; tray icon may not appear"
                );
            }
            true
        }
    }

    pub struct TrayHandle {
        inner: Option<ksni::blocking::Handle<AlcedoTray>>,
    }

    impl TrayHandle {
        fn inactive() -> Self {
            Self { inner: None }
        }
    }

    pub fn start_tray(
        show_fn: impl Fn() + Send + Sync + 'static,
        quit_fn: impl Fn() + Send + Sync + 'static,
    ) -> TrayHandle {
        if !settings::close_to_tray() {
            return TrayHandle::inactive();
        }

        let show = Arc::new(show_fn);
        let quit = Arc::new(quit_fn);
        let icon_name = "dev.alcedo.Mail".to_string();
        let tray = AlcedoTray::new(show, quit, icon_name);

        let disable_dbus_name = std::env::var("FLATPAK_ID").is_ok();
        match tray
            .assume_sni_available(true)
            .disable_dbus_name(disable_dbus_name)
            .spawn()
        {
            Ok(handle) => TrayHandle {
                inner: Some(handle),
            },
            Err(error) => {
                tracing::warn!(%error, "failed to start system tray");
                TrayHandle::inactive()
            }
        }
    }

    pub fn set_tray_unread(handle: &TrayHandle, unread: u32) {
        if let Some(inner) = &handle.inner {
            inner.update(|tray| {
                tray.unread_count.store(unread, Ordering::Relaxed);
            });
        }
    }

    pub fn stop_tray(handle: &mut Option<TrayHandle>) {
        if let Some(tray) = handle.take()
            && let Some(inner) = tray.inner
        {
            inner.shutdown().wait();
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    pub struct TrayHandle;

    pub fn start_tray(
        _show_fn: impl Fn() + Send + Sync + 'static,
        _quit_fn: impl Fn() + Send + Sync + 'static,
    ) -> TrayHandle {
        TrayHandle
    }

    pub fn set_tray_unread(_handle: &TrayHandle, _unread: u32) {}

    pub fn stop_tray(_handle: &mut Option<TrayHandle>) {}
}

pub use imp::{TrayHandle, set_tray_unread, start_tray, stop_tray};
