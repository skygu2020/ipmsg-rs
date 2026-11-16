use std::collections::HashMap;
use std::net::UdpSocket;
use async_channel::{Sender, Receiver};
use gtk::prelude::*;
use gtk::{CellRendererPixbuf, CellRendererText, Label, ListStore, ScrolledWindow, TreeView, TreeViewColumn};
use log::info;
use crate::ui::chat_window::ChatWindow;
use crate::events::model::model_run;
use crate::models::event::ModelEvent;
use crate::core::GLOBLE_SENDER;
use crate::models::event::UiEvent;

pub struct MainWindow {}

impl MainWindow {
    pub fn new(application: &adw::Application) -> MainWindow {
        // let (tx, rx): (glib::Sender<UiEvent>, glib::Receiver<UiEvent>) = MainContext::channel::<UiEvent>(Priority::HIGH);
        let (tx, rx): (Sender<UiEvent>, Receiver<UiEvent>) = async_channel::bounded(100);
        // let (model_sender, model_receiver): (crossbeam_channel::Sender<ModelEvent>, crossbeam_channel::Receiver<ModelEvent>) = unbounded();

        let window = gtk::ApplicationWindow::new(application);
        window.set_title(Some("飞鸽传书"));
        // window.set_position(gtk::WindowPosition::Center);
        window.set_default_size(960, 680);
        window.set_resizable(true);
        window.add_css_class("main-window");
        /*window.connect_delete_event(clone!(
        #[weak] window,
        #[upgrade_or]
        Inhibit(false),
        move |_, _| {
            unsafe {
                &window.destroy();
            }
            return Inhibit(false);
        },
    ));*/
        window.connect_close_request(move |win| {
            if let Some(application) = win.application() {
                application.remove_window(win);
            }
            glib::Propagation::Stop
        });

        // Modern two-pane contact layout.
        let header_bar = gtk::HeaderBar::new();
        let title_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let title = Label::new(Some("飞鸽传书"));
        title.add_css_class("title");
        let subtitle = Label::new(Some("局域网即时通讯"));
        subtitle.add_css_class("dim-label");
        subtitle.add_css_class("caption");
        title_box.append(&title);
        title_box.append(&subtitle);
        header_bar.set_title_widget(Some(&title_box));
        window.set_titlebar(Some(&header_bar));

        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.add_css_class("background");

        let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 8);
        sidebar.set_width_request(280);
        sidebar.set_margin_top(16);
        sidebar.set_margin_bottom(12);
        sidebar.set_margin_start(12);
        sidebar.set_margin_end(12);

        let contacts_heading = Label::new(Some("联系人"));
        contacts_heading.set_xalign(0.0);
        contacts_heading.add_css_class("title-2");
        let contacts_hint = Label::new(Some("当前局域网内的在线用户"));
        contacts_hint.set_xalign(0.0);
        contacts_hint.add_css_class("dim-label");
        contacts_hint.add_css_class("caption");
        sidebar.append(&contacts_heading);
        sidebar.append(&contacts_hint);

        let label = Label::new(Option::from(""));
        label.set_xalign(0.0);
        label.add_css_class("dim-label");
        label.add_css_class("caption");

        let scrolled = ScrolledWindow::new();
        scrolled.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
        scrolled.set_vexpand(true);
        scrolled.set_hexpand(true);
        scrolled.set_min_content_height(300);
        scrolled.add_css_class("card");

        let tree = create_and_setup_view();
        let model = create_and_fill_model();
        tree.set_model(Some(&model));
        tree.add_css_class("navigation-sidebar");
        scrolled.set_child(Some(&tree));
        sidebar.append(&scrolled);
        sidebar.append(&label);

        let separator = gtk::Separator::new(gtk::Orientation::Vertical);
        let welcome = gtk::Box::new(gtk::Orientation::Vertical, 12);
        welcome.set_hexpand(true);
        welcome.set_vexpand(true);
        welcome.set_halign(gtk::Align::Center);
        welcome.set_valign(gtk::Align::Center);
        let welcome_icon = gtk::Image::from_icon_name("network-workgroup-symbolic");
        welcome_icon.set_pixel_size(64);
        welcome_icon.add_css_class("dim-label");
        let welcome_title = Label::new(Some("飞鸽传书"));
        welcome_title.add_css_class("title-1");
        let welcome_text = Label::new(Some("选择左侧联系人，双击即可开始聊天"));
        welcome_text.add_css_class("dim-label");
        welcome.append(&welcome_icon);
        welcome.append(&welcome_title);
        welcome.append(&welcome_text);

        root.append(&sidebar);
        root.append(&separator);
        root.append(&welcome);
        GLOBLE_SENDER.send(ModelEvent::UserListSelected(String::from("未选择"))).unwrap();

        tree.connect_cursor_changed( move |tree_view| {
            let selection = tree_view.selection();
            if let Some((model, iter)) = selection.selected() {
                let str1 = model.get_value(&iter, 1).get::<String>().unwrap();
                GLOBLE_SENDER.send(ModelEvent::UserListSelected(str1)).unwrap();
            }
        });

        let mut chat_windows: HashMap<String, ChatWindow> = HashMap::new();

        tree.connect_row_activated(move |tree_view, _tree_path, _tree_view_column| {
            let selection = tree_view.selection();
            if let Some((model, iter)) = selection.selected() {
                let ip_str = model.get_value(&iter, 4).get::<String>().unwrap();
                let name = model.get_value(&iter, 1).get::<String>().unwrap();
                GLOBLE_SENDER.send(ModelEvent::UserListDoubleClicked{name, ip: ip_str }).unwrap();
            }
        });

        let socket: UdpSocket = match UdpSocket::bind(crate::constants::protocol::ADDR.as_str()) {
            Ok(s) => {
                info!("udp server start listening! {:?}", crate::constants::protocol::ADDR.as_str());
                s
            }
            Err(e) => panic!("couldn't bind socket: {}", e)
        };

        model_run(socket.try_clone().unwrap(),  tx);

        glib::spawn_future_local(async move {
            while let Ok(event) = rx.recv().await {
                match event {
                    UiEvent::OpenOrReOpenChatWindow { name, ip } => {
                        info!("winds 的长度 {:?}", chat_windows.len());
                        if !chat_windows.contains_key(&ip) {
                            let chat_win = crate::ui::chat_window::create_chat_window(name, ip.clone());
                            chat_windows.insert(ip.clone(), chat_win);
                        }
                    }
                    UiEvent::UpdateUserListFooterStatus(text) => {
                        label.set_text(&format!("-- {} --", text));
                    }
                    UiEvent::UserListRemoveOne(ip) => {
                        if let Some(first) = model.iter_first() {//拿出来第一条
                            let mut num: u32 = model.string_from_iter(&first).unwrap().parse::<u32>().unwrap();//序号 会改变
                            let ip1 = model.get_value(&first, 4).get::<String>().unwrap();//获取ip
                            if ip == ip1 {
                                model.remove(&first);
                            } else {
                                loop {
                                    num = num + 1;
                                    if let Some(next_iter) = model.iter_from_string(&num.to_string()) {
                                        let next_ip = model.get_value(&next_iter, 4).get::<String>().unwrap();//获取ip
                                        if next_ip == ip1 {
                                            model.remove(&next_iter);
                                            break;
                                        }
                                    } else {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    UiEvent::UserListAddOne(income_user) => {
                        let mut in_flag = false;
                        if let Some(first) = model.iter_first() {//拿出来第一条
                            let mut num: u32 = model.string_from_iter(&first).unwrap().parse::<u32>().unwrap();//序号 会改变
                            let ip = model.get_value(&first, 4).get::<String>().unwrap();//获取ip
                            if ip == income_user.ip {
                                in_flag = true;
                            } else {
                                loop {
                                    num = num + 1;
                                    if let Some(next_iter) = model.iter_from_string(&num.to_string()) {
                                        let next_ip = model.get_value(&next_iter, 4).get::<String>().unwrap();//获取ip
                                        if next_ip == income_user.ip {
                                            in_flag = true;
                                            break;
                                        }
                                    } else {
                                        break;
                                    }
                                }
                            }
                        }
                        if !in_flag {
                            //model.insert_with_values(None, &[0, 1, 2, 3], &[&&income_user.name, &&income_user.group, &&income_user.host, &&income_user.ip]);
                            let display_name = income_user.name
                                .split('\0')
                                .next()
                                .unwrap_or("")
                                .trim()
                                .to_owned();
                            model.insert_with_values(None, &[(0, &"face-smile"), (1, &display_name), (2, &&income_user.group), (3, &&income_user.host), (4, &&income_user.ip)]);
                        }
                    }
                    UiEvent::CloseChatWindow(ip) => {
                        chat_windows.remove(&ip);
                    }
                    UiEvent::OpenOrReOpenChatWindow1 { name, ip, packet: _ } => {
                        match chat_windows.get(&ip) {
                            Some(win) => {
                                win.win.show();
                            }
                            None => {
                                let chat_win = crate::ui::chat_window::create_chat_window(name, ip.clone());
                                chat_win.win.show();
                                chat_windows.insert(ip.clone(), chat_win);

                            }
                        }
                    }
                    UiEvent::DisplaySelfSendMsgInHis { to_ip, context, files: _ } => {
                        match chat_windows.get(&to_ip) {
                            Some(win) => {
                                crate::ui::chat_window::append_chat_message(
                                    &win.history_list, "我", &context, true,
                                );
                            }
                            None => {}
                        }
                    }
                    UiEvent::DisplayReceivedMsgInHis { from_ip, name, context, files } => {
                        match chat_windows.get(&from_ip) {
                            Some(win) => {
                                let (_, mut his_end_iter) = win.his_view.buffer().bounds();
                                win.his_view.buffer().insert(&mut his_end_iter, format!("{}:{}\n", name, context).as_str());

                                for file in &files {
                                    //win.received_store.insert_with_values(None, &[0, 1, 2, 3, 4, 5], &[&&file.name, &&file.file_id, &&file.packet_id, &&file.attr, &&file.size, &&file.mtime]);
                                    win.received_store.insert_with_values(None, &[(0, &&file.name), (1, &&file.file_id), (2, &&file.packet_id), (3, &&file.attr), (4, &&file.size), (5, &&file.mtime)]);
                                }
                            }
                            None => {}
                        }
                    }
                    UiEvent::RemoveInReceivedList { packet_id, file_id, download_ip } => {
                        match chat_windows.get(&download_ip) {
                            Some(win) => {
                                let pre_receive_file_store = &win.received_store;
                                if let Some(first) = pre_receive_file_store.iter_first() {
                                    let mut num: u32 = pre_receive_file_store.string_from_iter(&first).unwrap().parse::<u32>().unwrap();//序号 会改变
                                    let received_file_id = pre_receive_file_store.get_value(&first, 1).get::<u32>().unwrap();
                                    let received_packet_id = pre_receive_file_store.get_value(&first, 2).get::<u32>().unwrap();
                                    if file_id == received_file_id && packet_id == received_packet_id {
                                        pre_receive_file_store.remove(&first);
                                    } else {
                                        loop {
                                            num = num + 1;
                                            if let Some(next_iter) = pre_receive_file_store.iter_from_string(&num.to_string()) {
                                                let next_file_id = pre_receive_file_store.get_value(&next_iter, 1).get::<u32>().unwrap();
                                                let next_packet_id = pre_receive_file_store.get_value(&next_iter, 2).get::<u32>().unwrap();
                                                if next_file_id == file_id && next_packet_id == packet_id {
                                                    pre_receive_file_store.remove(&next_iter);
                                                    break;
                                                }
                                            } else {
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                            None => {}
                        }
                    }
                };
            }
        });
        window.set_child(Some(&root));
        window.present();
        MainWindow {}
    }
}

fn create_and_setup_view() -> TreeView {
    // Creating the tree view.
    let tree = TreeView::new();
    {
        let renderer = CellRendererPixbuf::new();
        let cell = CellRendererText::new();
        let col = TreeViewColumn::new();
        col.pack_start(&renderer, false);
        col.pack_end(&cell, false);
        // renderer.set_icon_name(Some("face-smile"));
        col.add_attribute(&renderer, "icon-name", 0);
        col.add_attribute(&cell, "text", 1);
        tree.append_column(&col);
    }
    tree.set_headers_visible(false);
    tree
}

fn create_and_fill_model() -> ListStore {
    // Creation of a model with two rows.
    let model = ListStore::new(&[String::static_type(), String::static_type(), String::static_type(), String::static_type(), String::static_type()]);
    model
}