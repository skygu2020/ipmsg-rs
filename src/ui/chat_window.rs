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
    pub pre_send_files :Arc<RefCell<Vec<model::FileInfo>>>,
    pub received_store :ListStore,
}

pub fn create_chat_window<S: Into<String>>(name :S, host_ip :S) -> ChatWindow {
    let name: String = name.into();
    let host_ip: String = host_ip.into();
    let chat_title = &format!("和{}({})聊天窗口", name, host_ip);

    let glade_src = include_str!("chat_window.ui");
    let builder = Builder::new();
    builder.add_from_string(glade_src).unwrap();

    let chat_window: Window = builder.object("chat_window").unwrap();
    chat_window.set_title(Some(chat_title));
    install_chat_message_styles(&chat_window);

    // Show the actual peer identity in the in-window conversation header too.
    let conversation_title: Label = builder.object("conversation_title").unwrap();
    conversation_title.set_text(&name);
    let conversation_subtitle: Label = builder.object("conversation_subtitle").unwrap();
    conversation_subtitle.set_text(&format!("{} · 局域网联系人", host_ip));
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
    append_column(&tree_view_received, 0, "收到的文件");

    list_box_presend.set_selection_mode(gtk::SelectionMode::None);
    list_box_presend.set_tooltip_text(Some("可将文件或文件夹拖放到此处添加到待发送列表"));


    let btn_clear: Button = builder.object("btn_clear").unwrap();
    let btn_send: Button = builder.object("btn_send").unwrap();//btn_file
    let btn_file: Button = builder.object("btn_file").unwrap();
    let btn_dir: Button = builder.object("btn_dir").unwrap();

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
                    let file = d.file().expect("Couldn't get file");
                    let save_base_path: PathBuf = file.path().expect("Couldn't get file path");
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
        let file_chooser_tmp = file_chooser.clone();
        let list_box_presend = list_box_presend.clone();
        file_chooser.connect_response(move |d: &gtk::FileChooserDialog, response: gtk::ResponseType| {
            if response == gtk::ResponseType::Ok {
                let filename: PathBuf = file_chooser_tmp.clone().file().unwrap().path().unwrap();
                let metadata: Metadata = fs::metadata(&filename).unwrap();
                let size = metadata.len();
                let attr = if metadata.is_file() {
                    crate::constants::protocol::IPMSG_FILE_REGULAR
                }else if metadata.is_dir() {
                    crate::constants::protocol::IPMSG_FILE_DIR
                }else {
                    panic!("oh no!");
                };
                let modify_time: time::SystemTime = metadata.modified().unwrap();
                let chrono_time = crate::util::system_time_to_date_time(modify_time);
                let local_time = chrono_time.with_timezone(&::chrono::Local);
                let name = filename.file_name().unwrap().to_str().unwrap();
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
        let file_chooser_tmp = file_chooser.clone();
        let list_box_presend = list_box_presend.clone();
        file_chooser.connect_response(move |d: &gtk::FileChooserDialog, response: gtk::ResponseType| {
                if response == gtk::ResponseType::Ok {
                    let filename: PathBuf = file_chooser_tmp.file().unwrap().path().unwrap();
                    let metadata: Metadata = fs::metadata(&filename).unwrap();
                    let size = metadata.len();
                    let attr = if metadata.is_file() {
                        crate::constants::protocol::IPMSG_FILE_REGULAR
                    }else if metadata.is_dir() {
                        crate::constants::protocol::IPMSG_FILE_DIR
                    }else {
                        panic!("oh no!");
                    };
                    let modify_time: time::SystemTime = metadata.modified().unwrap();
                    let chrono_time = crate::util::system_time_to_date_time(modify_time);
                    let local_time = chrono_time.with_timezone(&::chrono::Local);
                    let name = filename.file_name().unwrap().to_str().unwrap();
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
        pre_send_files,
        received_store: pre_received_files_model,
    }
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
    let row = ListBoxRow::new();
    let content = GtkBox::new(Orientation::Horizontal, 8);
    content.set_margin_top(4);
    content.set_margin_bottom(4);
    content.set_margin_start(6);
    content.set_margin_end(6);

    let label = Label::new(Some(&file_info.name));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    label.set_tooltip_text(Some(&file_info.file_name.to_string_lossy()));
    let cancel = Button::with_label("取消");
    cancel.add_css_class("flat");
    cancel.add_css_class("destructive-action");
    cancel.set_tooltip_text(Some("从待发送列表中移除此项目"));
    content.append(&label);
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
        }
    ));
    list_box.append(&row);
}

fn clear_queued_rows(list_box: &ListBox) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }
}

fn append_column(tree: &TreeView, id: i32, title: &str) {
    let column = TreeViewColumn::new();
    let cell = CellRendererText::new();
    column.pack_start(&cell, true);
    column.set_title(title);
    column.add_attribute(&cell, "text", id);
    tree.append_column(&column);
    tree.set_headers_visible(true);
}

fn create_and_fill_model1() -> ListStore {
    let model = ListStore::new(&[String::static_type(), u32::static_type(), u32::static_type(), u8::static_type(), u64::static_type(), i64::static_type()]);
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
        ".message-history row { background: transparent; padding: 0; }\\
         .message-history row:hover { background: transparent; }\\
         .message-bubble { border-radius: 16px; padding: 10px 14px; }\\
         .message-incoming { background-color: @card_bg_color; }\\
         .message-outgoing { background-color: alpha(@accent_bg_color, 0.18); }"
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
        .and_then(|previous| previous.tooltip_text())
        .map(|previous_key| previous_key.as_str() == group_key.as_str())
        .unwrap_or(false);

    let row = ListBoxRow::new();
    row.set_selectable(false);
    row.set_activatable(false);
    row.set_tooltip_text(Some(&group_key));

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
