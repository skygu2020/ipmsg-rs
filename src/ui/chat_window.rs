use gtk::prelude::*;
use gtk::{
    CellRendererText, Window, ListStore, TreeView, TreeViewColumn, Builder, Button, TextView, WrapMode, ListBox, ListBoxRow, Label, Box as GtkBox, Orientation, Align
};
use std::sync::Arc;
use std::cell::RefCell;
use std::path::PathBuf;
use std::fs::{self, Metadata};
use std::time::{self};
use chrono::prelude::*;
use log::info;
use glib::clone;
use crate::core::GLOBLE_SENDER;
use crate::models::model::{self, ReceivedSimpleFileInfo};
use crate::models::event::ModelEvent;
use crate::models::message;
//use crate::app::GLOBAL_CHATWINDOWS;

// make moving clones into closures more convenient


#[derive(Clone)]
pub struct ChatWindow {
    pub win :Window,
    pub history_list: ListBox,
    pub history_scroller: gtk::ScrolledWindow,
    pub ip :String,
    pub host: String,
    pub pre_send_files :Arc<RefCell<Vec<model::FileInfo>>>,
    pub received_store :ListStore,
    pub conversation_title: Label,
    pub conversation_subtitle: Label,
}

impl ChatWindow {
    /// Refresh the peer name in both the native title bar and conversation header.
    pub fn update_peer_identity(&self, name: &str, host: &str) {
        self.win.set_title(Some(&format!("和{}({})聊天窗口", name, self.ip)));
        self.conversation_title.set_text(name);
        self.conversation_subtitle.set_text(&format!("主机名：{} · {}", host, self.ip));
    }
}

pub fn create_chat_window<S: Into<String>>(name: S, host_ip: S, host_name: S) -> ChatWindow {
    let name: String = name.into();
    let host_ip: String = host_ip.into();
    let host_name: String = host_name.into();
    let chat_title = &format!("和{}({})聊天窗口", name, host_ip);

    let glade_src = include_str!("chat_window.ui");
    let builder = Builder::new();
    builder.add_from_string(glade_src).unwrap();

    let chat_window: Window = builder.object("chat_window").unwrap();
    chat_window.set_title(Some(chat_title));
    install_chat_message_styles(&chat_window);

    // Use the same GTK client-side title bar as the main window.
    chat_window.add_css_class("main-window");
    let header_bar = gtk::HeaderBar::new();
    let title_box = GtkBox::new(Orientation::Vertical, 0);
    let conversation_title = Label::new(Some(&name));
    conversation_title.add_css_class("title");
    let conversation_subtitle = Label::new(Some(&format!("主机名：{} · {}", host_name, host_ip)));
    conversation_subtitle.add_css_class("dim-label");
    conversation_subtitle.add_css_class("caption");
    title_box.append(&conversation_title);
    title_box.append(&conversation_subtitle);
    header_bar.set_title_widget(Some(&title_box));
    chat_window.set_titlebar(Some(&header_bar));
    // chat_window.set_border_width(5);
    // 消息历史使用独立行和气泡布局。
    let history_list: ListBox = builder.object("history_list").unwrap();
    let history_scroller: gtk::ScrolledWindow = builder.object("history_scroller").unwrap();
    history_list.set_selection_mode(gtk::SelectionMode::None);
    //待发送的
    let text_view_presend: TextView = builder.object("text_view_presend").unwrap();
    //待发送文件
    let list_box_presend: ListBox = builder.object("list_box_presend").unwrap();
    //接受的文件
    let tree_view_received: TreeView = builder.object("tree_view_received").unwrap();

    text_view_presend.set_wrap_mode(WrapMode::WordChar);
    // Reserve a little extra ascent space for glyphs with tall ascenders (for example, lowercase 'f').
    text_view_presend.set_pixels_above_lines(4);
    tree_view_received.set_headers_visible(true);
    tree_view_received.set_enable_search(true);
    tree_view_received.set_search_column(0);
    tree_view_received.set_tooltip_text(Some("双击文件或文件夹，选择保存位置"));
    tree_view_received.selection().set_mode(gtk::SelectionMode::Single);
    append_column(&tree_view_received, 0, "文件名");
    append_column(&tree_view_received, 6, "大小");
    append_column(&tree_view_received, 7, "进度");
    append_column(&tree_view_received, 8, "状态");

    list_box_presend.set_selection_mode(gtk::SelectionMode::None);
    list_box_presend.set_tooltip_text(Some("可将文件或文件夹拖放到此处添加到待发送列表"));
    show_queued_files_empty_state(&list_box_presend);


    let btn_clear: Button = builder.object("btn_clear").unwrap();
    let btn_send: Button = builder.object("btn_send").unwrap();//btn_file
    let btn_file: Button = builder.object("btn_file").unwrap();
    let btn_dir: Button = builder.object("btn_dir").unwrap();

    // Enter sends the message; Shift+Enter keeps the normal newline behavior.
    let send_key_controller = gtk::EventControllerKey::new();
    send_key_controller.connect_key_pressed(glib::clone!(
        #[strong] btn_send,
        move |_, key, _, state| {
            let is_enter = key == gdk::Key::Return || key == gdk::Key::KP_Enter;
            if is_enter && !state.contains(gdk::ModifierType::SHIFT_MASK) {
                btn_send.emit_clicked();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        }
    ));
    text_view_presend.add_controller(send_key_controller);

    //let text_view_presend_clone = text_view_presend.clone();
    //let arc_received_files: Arc<RefCell<Vec<ReceivedSimpleFileInfo>>> = Arc::new(RefCell::new(Vec::new()));
    let pre_send_files: Arc<RefCell<Vec<model::FileInfo>>> = Arc::new(RefCell::new(Vec::new()));//待发送文件列表


    let pre_received_files_model = create_and_fill_model1();
    tree_view_received.set_model(Some(&pre_received_files_model));
    let files_send_clone = pre_send_files.clone();
    // Accept one or more local files/directories dragged from the file manager.
    // GTK 4.6+ exposes dropped file collections through GdkFileList.
    let drop_target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    drop_target.connect_drop(clone!(
        #[strong] pre_send_files,
        #[strong] list_box_presend,
        move |_, value, _, _| {
            let Ok(file_list) = value.get::<gdk::FileList>() else {
                return false;
            };

            let mut added = 0usize;
            for file in file_list.files() {
                let Some(path) = file.path() else {
                    continue;
                };
                match add_path_to_send_list(path, &pre_send_files, &list_box_presend) {
                    Ok(()) => added += 1,
                    Err(err) => log::warn!("无法添加拖放项目: {}", err),
                }
            }
            added > 0
        }
    ));
    chat_window.add_controller(drop_target);

    btn_send.connect_clicked(clone!(
        #[strong] list_box_presend,
        #[strong] host_ip,
        #[strong] text_view_presend,
        move|_|{
        let (start_iter, end_iter) = text_view_presend.buffer().bounds();
        let context :&str = &text_view_presend.buffer().text(&start_iter, &end_iter, false);
        let (packet, share_file) = message::create_sendmsg(context.to_owned(), files_send_clone.clone().borrow().to_vec(), host_ip.clone());
        GLOBLE_SENDER.send(ModelEvent::SendOneMsg {to_ip: host_ip.clone(), packet, context: context.to_owned(), files: share_file}).unwrap();
        files_send_clone.borrow_mut().clear();
        clear_queued_rows(&list_box_presend);
        text_view_presend.buffer().set_text("");
    },
    ));

    let chat_window_open_save = chat_window.clone();
    tree_view_received.connect_row_activated(clone!(
        #[weak] chat_window_open_save,
        #[strong] host_ip,
        move |tree_view, _tree_path, _tree_view_column| {
        let selection = tree_view.selection();
        if let Some((model, iter)) = selection.selected() {
            let name: String = model.get_value(&iter, 0).get().unwrap();
            let fid: u32 = model.get_value(&iter, 1).get().unwrap();
            let pid: u32 = model.get_value(&iter, 2).get().unwrap();
            let file_type: u8 = model.get_value(&iter, 3).get().unwrap();
            let size: u64 = model.get_value(&iter, 4).get().unwrap();
            let mtime: i64 = model.get_value(&iter, 5).get().unwrap();

            let file_chooser = gtk::FileChooserDialog::new(
                Some("保存文件"),
                Some(&chat_window_open_save),
                gtk::FileChooserAction::SelectFolder,
                &[("保存", gtk::ResponseType::Ok), ("取消", gtk::ResponseType::Cancel)],
            );

            let host_ip = host_ip.clone();
            file_chooser.connect_response(move |d: &gtk::FileChooserDialog, response: gtk::ResponseType| {
                if response == gtk::ResponseType::Ok {
                    let Some(save_base_path) = selected_local_path(d) else {
                        log::warn!("未能获取保存目录");
                        d.close();
                        return;
                    };
                    info!("choosed {:?} {:?} {:?} {:?} {:?}", name, fid, pid, save_base_path, file_type);
                    if let Err(err) = GLOBLE_SENDER.send(ModelEvent::PutDownloadTaskInPool{ file: ReceivedSimpleFileInfo{
                        file_id: fid,
                        packet_id: pid,
                        name: name.clone(),
                        attr: file_type,
                        size,
                        mtime
                    }, save_base_path, download_ip: host_ip.clone() }) {
                        log::warn!("failed to queue file download: {}", err);
                    }
                }
                d.close();
            });
            file_chooser.show();
        }
    },
    ));

    btn_clear.connect_clicked(clone!(
        #[strong] text_view_presend,
        move|_|{
        text_view_presend.buffer().set_text("");
    },
    ));

    let pre_send_files_open_file = pre_send_files.clone();
    let chat_window_open_save = chat_window_open_save.clone();
    btn_file.connect_clicked(clone!(
        #[strong] list_box_presend,
        #[weak] chat_window_open_save,
        move|_|{

        let file_chooser = gtk::FileChooserDialog::new(
                Some("打开文件"),
                Some(&chat_window_open_save),
                gtk::FileChooserAction::Open,
                &[("选择文件", gtk::ResponseType::Ok), ("取消", gtk::ResponseType::Cancel)],
            );

        let pre_send_files_open_file = pre_send_files_open_file.clone();
        let list_box_presend = list_box_presend.clone();
        file_chooser.connect_response(move |d: &gtk::FileChooserDialog, response: gtk::ResponseType| {
            if response == gtk::ResponseType::Ok {
                let Some(filename) = selected_local_path(d) else {
                    log::warn!("未能获取所选文件的本地路径");
                    d.close();
                    return;
                };
                let metadata: Metadata = match fs::metadata(&filename) {
                    Ok(metadata) => metadata,
                    Err(err) => {
                        log::warn!("无法读取文件信息 {:?}: {}", filename, err);
                        d.close();
                        return;
                    }
                };
                let size = metadata.len();
                let attr = if metadata.is_file() {
                    crate::constants::protocol::IPMSG_FILE_REGULAR
                } else if metadata.is_dir() {
                    crate::constants::protocol::IPMSG_FILE_DIR
                } else {
                    log::warn!("不支持的文件类型: {:?}", filename);
                    d.close();
                    return;
                };
                let modify_time = metadata.modified().unwrap_or_else(|_| time::SystemTime::now());
                let chrono_time = crate::util::system_time_to_date_time(modify_time);
                let local_time = chrono_time.with_timezone(&::chrono::Local);
                let Some(name) = filename.file_name().and_then(|name| name.to_str()) else {
                    log::warn!("文件名无法按 UTF-8 显示: {:?}", filename);
                    d.close();
                    return;
                };
                let file_info = model::FileInfo {
                    file_id: Local::now().timestamp() as u32,
                    file_name: filename.clone(),
                    name: name.to_owned(),
                    attr: attr as u8,
                    size: size,
                    mtime: local_time.time(),
                    atime: Local::now().time(),
                    crtime: Local::now().time()
                };
                append_queued_file(&list_box_presend, &pre_send_files_open_file, file_info);
            }
            d.close();
        });
        file_chooser.show();
    },
    ));

    let chat_window_open_save = chat_window_open_save.clone();
    btn_dir.connect_clicked(clone!(
        #[strong] list_box_presend,
        #[strong] pre_send_files,
        #[weak] chat_window_open_save,
        move|_|{
        let file_chooser = gtk::FileChooserDialog::new(
                Some("打开文件夹"),
                Some(&chat_window_open_save),
                gtk::FileChooserAction::SelectFolder,
                &[("选择文件夹", gtk::ResponseType::Ok), ("取消", gtk::ResponseType::Cancel)],
            );
        let pre_send_files_open_dir = pre_send_files.clone();
        let list_box_presend = list_box_presend.clone();
        file_chooser.connect_response(move |d: &gtk::FileChooserDialog, response: gtk::ResponseType| {
                if response == gtk::ResponseType::Ok {
                    let Some(filename) = d.file().and_then(|file| file.path()) else {
                        log::warn!("未能获取所选文件夹的本地路径");
                        d.close();
                        return;
                    };
                    let metadata: Metadata = match fs::metadata(&filename) {
                        Ok(metadata) => metadata,
                        Err(err) => {
                            log::warn!("无法读取文件夹信息 {:?}: {}", filename, err);
                            d.close();
                            return;
                        }
                    };
                    let size = metadata.len();
                    let attr = if metadata.is_file() {
                        crate::constants::protocol::IPMSG_FILE_REGULAR
                    } else if metadata.is_dir() {
                        crate::constants::protocol::IPMSG_FILE_DIR
                    } else {
                        log::warn!("不支持的文件类型: {:?}", filename);
                        d.close();
                        return;
                    };
                    let modify_time = metadata.modified().unwrap_or_else(|_| time::SystemTime::now());
                    let chrono_time = crate::util::system_time_to_date_time(modify_time);
                    let local_time = chrono_time.with_timezone(&::chrono::Local);
                    let Some(name) = filename.file_name().and_then(|name| name.to_str()) else {
                        log::warn!("文件夹名称无法按 UTF-8 显示: {:?}", filename);
                        d.close();
                        return;
                    };
                    let file_info = model::FileInfo {
                        file_id: Local::now().timestamp() as u32,
                        file_name: filename.clone(),
                        name: name.to_owned(),
                        attr: attr as u8,
                        size: size,
                        mtime: local_time.time(),
                        atime: Local::now().time(),
                        crtime: Local::now().time(),
                    };
                    append_queued_file(&list_box_presend, &pre_send_files_open_dir, file_info);
                }
                d.close();
        });
        file_chooser.show();
    },
    ));

    chat_window.connect_close_request(clone!(
        #[strong] host_ip,
        move |window| {
        GLOBLE_SENDER.send(ModelEvent::ClickChatWindowCloseBtn{from_ip: host_ip.clone()}).unwrap();
        /*if let Some(application) = window.application() {
            application.remove_window(window);*/
            window.destroy();
            info!("关闭聊天窗口！");
        // }
        glib::Propagation::Proceed
    },
    ));


    // chat_window.show_all();
    chat_window.present();
    let clone_chat = chat_window.clone();
    ChatWindow {
        win: clone_chat,
        history_list,
        history_scroller,
        ip: host_ip,
        host: host_name,
        pre_send_files,
        received_store: pre_received_files_model,
        conversation_title,
        conversation_subtitle,
    }
}

/// Resolve the chooser selection through both GTK FileChooser accessors.
/// Some GTK backends expose the selected item in the files model even when
/// the single-file convenience accessor temporarily returns None.
fn selected_local_path(chooser: &gtk::FileChooserDialog) -> Option<PathBuf> {
    chooser.file()
        .and_then(|file| file.path())
        .or_else(|| {
            chooser.files()
                .item(0)
                .and_then(|item| item.downcast::<gio::File>().ok())
                .and_then(|file| file.path())
        })
}

fn add_path_to_send_list(
    path: PathBuf,
    files: &Arc<RefCell<Vec<model::FileInfo>>>,
    list_box: &ListBox,
) -> Result<(), String> {
    let metadata = fs::metadata(&path).map_err(|err| format!("{:?}: {}", path, err))?;
    let attr = if metadata.is_file() {
        crate::constants::protocol::IPMSG_FILE_REGULAR
    } else if metadata.is_dir() {
        crate::constants::protocol::IPMSG_FILE_DIR
    } else {
        return Err(format!("不支持的文件类型: {:?}", path));
    };

    let name = path.file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("文件名无法按 UTF-8 显示: {:?}", path))?
        .to_owned();
    let now = Local::now();
    let modified = metadata.modified().unwrap_or_else(|_| std::time::SystemTime::now());
    let modified_time = crate::util::system_time_to_date_time(modified)
        .with_timezone(&Local)
        .time();
    let file_id = (now.timestamp() as u32).wrapping_add(files.borrow().len() as u32);

    let file_info = model::FileInfo {
        file_id,
        file_name: path,
        name: name.clone(),
        attr: attr as u8,
        size: metadata.len(),
        mtime: modified_time,
        atime: now.time(),
        crtime: now.time(),
    };

    append_queued_file(list_box, files, file_info);
    Ok(())
}

fn append_queued_file(
    list_box: &ListBox,
    files: &Arc<RefCell<Vec<model::FileInfo>>>,
    file_info: model::FileInfo,
) {
    let file_id = file_info.file_id;
    if let Some(child) = list_box.first_child() {
        if child.widget_name() == "queued-files-empty" {
            list_box.remove(&child);
        }
    }
    let row = ListBoxRow::new();
    let content = GtkBox::new(Orientation::Horizontal, 8);
    content.set_margin_top(4);
    content.set_margin_bottom(4);
    content.set_margin_start(6);
    content.set_margin_end(6);

    let icon_name = if file_info.attr == crate::constants::protocol::IPMSG_FILE_DIR as u8 {
        "folder-symbolic"
    } else {
        "text-x-generic-symbolic"
    };
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.set_pixel_size(24);
    icon.add_css_class("dim-label");

    let details = GtkBox::new(Orientation::Vertical, 2);
    details.set_hexpand(true);
    let label = Label::new(Some(&file_info.name));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    label.set_tooltip_text(Some(&file_info.file_name.to_string_lossy()));
    let size_text = if file_info.attr == crate::constants::protocol::IPMSG_FILE_DIR as u8 {
        "文件夹".to_owned()
    } else {
        format_file_size(file_info.size)
    };
    let size_label = Label::new(Some(&size_text));
    size_label.set_xalign(0.0);
    size_label.add_css_class("dim-label");
    size_label.add_css_class("caption");
    details.append(&label);
    details.append(&size_label);

    let cancel = Button::with_label("移除");
    cancel.add_css_class("flat");
    cancel.add_css_class("destructive-action");
    cancel.set_tooltip_text(Some("从待发送列表中移除此项目"));
    content.append(&icon);
    content.append(&details);
    content.append(&cancel);
    row.set_child(Some(&content));

    files.borrow_mut().push(file_info);
    let files_for_cancel = files.clone();
    cancel.connect_clicked(glib::clone!(
        #[strong] files_for_cancel,
        #[weak] row,
        #[weak] list_box,
        move |_| {
            files_for_cancel.borrow_mut().retain(|file| file.file_id != file_id);
            list_box.remove(&row);
            if list_box.first_child().is_none() {
                show_queued_files_empty_state(&list_box);
            }
        }
    ));
    list_box.append(&row);
}

pub(crate) fn format_file_size(size: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = size as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", size, UNITS[unit])
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}

fn clear_queued_rows(list_box: &ListBox) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }
    show_queued_files_empty_state(list_box);
}

fn show_queued_files_empty_state(list_box: &ListBox) {
    let row = ListBoxRow::new();
    row.set_selectable(false);
    row.set_activatable(false);
    row.set_widget_name("queued-files-empty");

    let message = Label::new(Some("暂无待发送文件\n拖放文件到这里，或点击上方按钮添加"));
    message.set_xalign(0.5);
    message.set_justify(gtk::Justification::Center);
    message.set_wrap(true);
    message.set_margin_top(18);
    message.set_margin_bottom(18);
    message.set_margin_start(10);
    message.set_margin_end(10);
    message.add_css_class("dim-label");
    row.set_child(Some(&message));
    list_box.append(&row);
}

fn append_column(tree: &TreeView, id: i32, title: &str) {
    let column = TreeViewColumn::new();
    let cell = CellRendererText::new();
    if id == 7 {
        let progress = gtk::CellRendererProgress::new();
        column.set_sizing(gtk::TreeViewColumnSizing::Fixed);
        column.set_fixed_width(112);
        column.set_expand(false);
        column.set_resizable(false);
        column.pack_start(&progress, true);
        column.add_attribute(&progress, "value", id);
        column.set_title(title);
        tree.append_column(&column);
        return;
    }
    if id == 6 {
        cell.set_xalign(1.0);
        column.set_sizing(gtk::TreeViewColumnSizing::Fixed);
        column.set_fixed_width(76);
        column.set_expand(false);
    } else if id == 8 {
        cell.set_xalign(0.0);
        cell.set_ellipsize(gtk::pango::EllipsizeMode::None);
        column.set_sizing(gtk::TreeViewColumnSizing::Fixed);
        column.set_fixed_width(120);
        column.set_expand(false);
    } else {
        cell.set_xalign(0.0);
        column.set_expand(true);
    }
    column.set_resizable(true);
    column.pack_start(&cell, true);
    column.set_title(title);
    column.add_attribute(&cell, "text", id);
    tree.append_column(&column);
}

fn create_and_fill_model1() -> ListStore {
    let model = ListStore::new(&[
        String::static_type(), u32::static_type(), u32::static_type(),
        u8::static_type(), u64::static_type(), i64::static_type(),
        String::static_type(), i32::static_type(), String::static_type(),
    ]);
    model
}

/*
/// ip
fn modify_received_list(received_store :Option<ListStore>, received_files: Arc<RefCell<Vec<ReceivedSimpleFileInfo>>>) -> Continue {

    Continue(false)
}*/

/// Install theme-aware styling for incoming and outgoing message bubbles.
fn install_chat_message_styles(window: &Window) {
    let display = gtk::prelude::WidgetExt::display(window);
    let provider = gtk::CssProvider::new();
    provider.load_from_data(
        ".message-history row { background: transparent; padding: 0; }\
         .message-history row:hover { background: transparent; }\
         .message-bubble { border-radius: 16px; padding: 10px 14px; }\
         .message-incoming { background-color: @card_bg_color; }\
         .message-outgoing { background-color: alpha(@accent_bg_color, 0.18); }\
"
    );
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

/// Append one message as a chat bubble. Incoming messages align left; our own
/// messages align right. The timestamp is rendered separately from the body.
pub fn append_chat_message(
    history: &ListBox,
    scroller: &gtk::ScrolledWindow,
    sender: &str,
    body: &str,
    outgoing: bool,
) {
    history.add_css_class("message-history");

    let group_key = format!("{}|{}", sender, if outgoing { "out" } else { "in" });
    let continues_group = history.last_child()
        .and_then(|child| child.downcast::<ListBoxRow>().ok())
        .map(|previous| previous.widget_name().as_str() == group_key.as_str())
        .unwrap_or(false);

    let row = ListBoxRow::new();
    row.set_selectable(false);
    row.set_activatable(false);
    // Keep grouping metadata internal instead of exposing it as a hover tooltip.
    row.set_widget_name(&group_key);

    let line = GtkBox::new(Orientation::Horizontal, 0);
    line.set_hexpand(true);
    line.set_halign(if outgoing { Align::End } else { Align::Start });

    let bubble = GtkBox::new(Orientation::Vertical, 5);
    bubble.set_margin_top(if continues_group { 1 } else { 7 });
    bubble.set_margin_bottom(3);
    bubble.set_margin_start(8);
    bubble.set_margin_end(8);
    bubble.set_hexpand(false);
    bubble.add_css_class("message-bubble");
    bubble.add_css_class(if outgoing { "message-outgoing" } else { "message-incoming" });

    let timestamp = Local::now().format("%H:%M").to_string();
    let metadata_text = if continues_group {
        timestamp.clone()
    } else {
        format!("{}  ·  {}", sender, timestamp)
    };
    let metadata = Label::new(Some(&metadata_text));
    metadata.set_xalign(if outgoing { 1.0 } else { 0.0 });
    metadata.set_single_line_mode(true);
    metadata.set_ellipsize(gtk::pango::EllipsizeMode::End);
    metadata.set_max_width_chars(42);
    metadata.add_css_class("dim-label");
    metadata.add_css_class("caption");

    let content = Label::new(Some(body));
    content.set_xalign(0.0);
    content.set_wrap(true);
    content.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    content.set_selectable(true);
    content.set_max_width_chars(52);
    content.set_hexpand(true);

    bubble.append(&metadata);
    bubble.append(&content);
    line.append(&bubble);
    row.set_child(Some(&line));
    history.append(&row);

    // Wait until GTK recalculates the list's height before scrolling to the
    // newest message; setting the adjustment immediately can use stale bounds.
    let scroller = scroller.clone();
    glib::idle_add_local_once(move || {
        let adjustment = scroller.vadjustment();
        let bottom = (adjustment.upper() - adjustment.page_size()).max(adjustment.lower());
        adjustment.set_value(bottom);
    });
}
