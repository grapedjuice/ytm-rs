//! Now playing's Comments panel (Settings › Playback › Show comments): the playing song's
//! YouTube comments, Top or Newest, with their replies opening inline. Signed in, a box to
//! comment in, Reply on each comment, and likes.
//!
//! It all comes from music.youtube.com's own `next` calls: the song's Comments tab holds a
//! token for the first page, and every list ("N replies", "Show more…", Newest) has its own.
//! Comments come as entities (`frameworkUpdates`) that each thread points to by key.

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

use egui::{Color32, Rect, RichText, Sense, Ui, pos2, vec2};
use serde_json::{Value, json};

use super::App;
use super::widgets::{self, icon_at, text_at};
use crate::backend::Req;
use crate::theme;

const THUMBS_UP: char = '\u{e18a}';
const INDENT: f32 = 44.0;

#[derive(Clone)]
struct Comment {
    id: String,
    author: String,
    avatar: String,
    published: String,
    text: String,
    likes: String,
    /// The count once liked (YouTube sends both).
    likes_liked: String,
    replies: String,
    creator: bool,
    hearted: bool,
    liked: bool,
    /// Top-level only: the token for its replies.
    replies_token: Option<String>,
    /// Signed in only: what `comment/create_comment_reply` needs to answer it.
    reply_params: Option<String>,
    /// Signed in only: the `comment/perform_comment_action` actions to like and unlike it.
    like_action: Option<String>,
    unlike_action: Option<String>,
    /// Bytes of `text` that are a leading @mention (YouTube shows it as a link, with no
    /// space after it in the text itself).
    mention: usize,
}

#[derive(Clone)]
enum Node {
    Comment(Box<Comment>, u8),
    /// "Show more": the next page of this list.
    More { token: String, label: String, depth: u8 },
}

#[derive(Default, PartialEq)]
enum Load {
    #[default]
    Loading,
    Ready,
    /// The song has comments turned off.
    Off,
    Failed,
}

#[derive(Default)]
struct State {
    /// The song these are for: another one starts over.
    video: Option<String>,
    load: Load,
    count: String,
    /// Signed in only: what `comment/create_comment` needs.
    create_params: Option<String>,
    /// The tokens for Top and Newest, and which is showing.
    sort: [Option<String>; 2],
    newest: bool,
    /// The sort being fetched: its page replaces the list.
    sorting: Option<String>,
    root: Vec<Node>,
    /// A top-level comment's replies, by its id, once opened.
    threads: HashMap<String, Vec<Node>>,
    open: HashSet<String>,
    pending: HashSet<String>,
    failed: HashSet<String>,
    draft: String,
    reply_to: Option<String>,
    reply_draft: String,
    note: Option<String>,
    /// A comment or reply is on its way to YouTube.
    sending: bool,
    /// Likes clicked here, by comment id, over what YouTube sent.
    liked: HashMap<String, bool>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(Default::default);

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_owned()
}

/// The first string keyed `key` anywhere under `v`.
fn find_str(v: &Value, key: &str) -> Option<String> {
    match v {
        Value::Object(m) => m.get(key).and_then(Value::as_str).map(str::to_owned).or_else(|| m.values().find_map(|x| find_str(x, key))),
        Value::Array(a) => a.iter().find_map(|x| find_str(x, key)),
        _ => None,
    }
}

fn continuation_items(v: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    for ep in v["onResponseReceivedEndpoints"].as_array().into_iter().flatten() {
        for k in ["reloadContinuationItemsCommand", "appendContinuationItemsAction"] {
            out.extend(ep[k]["continuationItems"].as_array().into_iter().flatten().cloned());
        }
    }
    out
}

fn mutations(v: &Value) -> Vec<Value> {
    v["frameworkUpdates"]["entityBatchUpdate"]["mutations"].as_array().cloned().unwrap_or_default()
}

fn token_of(cir: &Value) -> Option<String> {
    cir["continuationEndpoint"]["continuationCommand"]["token"]
        .as_str()
        .or_else(|| cir["button"]["buttonRenderer"]["command"]["continuationCommand"]["token"].as_str())
        .map(str::to_owned)
}

fn comment(vm: &Value, muts: &[Value], replies_token: Option<String>) -> Option<Comment> {
    if vm["commentKey"].is_null() && !vm["commentViewModel"].is_null() {
        return comment(&vm["commentViewModel"], muts, replies_token);
    }
    let by_key = |k: &Value| k.as_str().and_then(|key| muts.iter().find(|m| m["entityKey"] == key)).map(|m| &m["payload"]);
    let e = &by_key(&vm["commentKey"])?["commentEntityPayload"];
    let state = by_key(&vm["toolbarStateKey"]).map(|t| &t["engagementToolbarStateEntityPayload"]);
    // Signed out, the toolbar's commands are sign-ins; signed in, Reply opens a dialog that
    // carries `createReplyParams`, and like / unlike are comment actions.
    let surface = by_key(&vm["toolbarSurfaceKey"]).map(|p| &p["engagementToolbarSurfaceEntityPayload"]);
    let action = |k: &str| surface.and_then(|p| p[k]["innertubeCommand"]["performCommentActionEndpoint"]["action"].as_str()).map(str::to_owned);
    Some(Comment {
        id: s(&vm["commentId"]),
        author: s(&e["author"]["displayName"]),
        avatar: s(&e["author"]["avatarThumbnailUrl"]),
        published: s(&e["properties"]["publishedTime"]),
        text: s(&e["properties"]["content"]["content"]),
        likes: s(&e["toolbar"]["likeCountNotliked"]),
        likes_liked: s(&e["toolbar"]["likeCountLiked"]),
        replies: s(&e["toolbar"]["replyCount"]),
        creator: e["author"]["isCreator"].as_bool().unwrap_or(false),
        hearted: state.is_some_and(|t| t["heartState"] == "TOOLBAR_HEART_STATE_HEARTED"),
        liked: state.is_some_and(|t| t["likeState"] == "TOOLBAR_LIKE_STATE_LIKED"),
        replies_token,
        reply_params: surface.and_then(|p| find_str(&p["replyCommand"], "createReplyParams")),
        like_action: action("likeCommand"),
        unlike_action: action("unlikeCommand"),
        mention: 0,
    })
}

/// `items` as rows at `depth`. A top-level thread keeps its replies behind a token; a
/// reply's own replies (YouTube nests one level deeper) come inline.
fn nodes(items: &[Value], muts: &[Value], depth: u8, out: &mut Vec<Node>) {
    for item in items {
        let t = &item["commentThreadRenderer"];
        let more = &item["continuationItemRenderer"];
        if !t.is_null() {
            let subs = t["replies"]["commentRepliesRenderer"]["subThreads"].as_array().map(Vec::as_slice).unwrap_or(&[]);
            let token = if depth == 0 { subs.iter().find_map(|x| token_of(&x["continuationItemRenderer"])) } else { None };
            if let Some(c) = comment(&t["commentViewModel"]["commentViewModel"], muts, token) {
                out.push(Node::Comment(Box::new(c), depth));
            }
            if depth > 0 {
                nodes(subs, muts, depth + 1, out);
            }
        } else if !item["commentViewModel"].is_null() {
            if let Some(c) = comment(&item["commentViewModel"], muts, None) {
                out.push(Node::Comment(Box::new(c), depth));
            }
        } else if let Some(token) = token_of(more) {
            let label = more["button"]["buttonRenderer"]["text"]["runs"][0]["text"].as_str().map(str::to_owned);
            let label = label.unwrap_or_else(|| if depth == 0 { "Show more comments" } else { "Show more replies" }.to_owned());
            out.push(Node::More { token, label, depth });
        }
    }
}

/// Split a reply's leading @mention of someone in the thread off its text.
fn mark_mentions(list: &mut [Node], authors: &[String]) {
    for n in list {
        let Node::Comment(c, d) = n else { continue };
        if *d == 0 || c.mention > 0 {
            continue;
        }
        let text = c.text.trim_start_matches('\u{200b}');
        let Some(m) = authors.iter().filter(|a| a.len() > 1 && text.starts_with(a.as_str())).max_by_key(|a| a.len()) else { continue };
        let rest = text[m.len()..].trim_start();
        c.text = format!("{m} {rest}");
        c.mention = m.len();
    }
}

/// A first page (the song's, or another sort's): it replaces the list.
fn fill(st: &mut State, v: &Value) {
    let items = continuation_items(v);
    for item in &items {
        let h = &item["commentsHeaderRenderer"];
        if let Some(runs) = h["countText"]["runs"].as_array() {
            st.count = runs.iter().filter_map(|r| r["text"].as_str()).collect();
            st.create_params = find_str(&h["createRenderer"], "createCommentParams");
            let subs = h["sortMenu"]["sortFilterSubMenuRenderer"]["subMenuItems"].as_array().map(Vec::as_slice).unwrap_or(&[]);
            for (slot, sub) in st.sort.iter_mut().zip(subs) {
                *slot = find_str(&sub["continuation"], "continuation").or_else(|| find_str(&sub["serviceEndpoint"], "token"));
            }
        }
    }
    st.root.clear();
    nodes(&items, &mutations(v), 0, &mut st.root);
    st.threads.clear();
    st.open.clear();
    st.reply_to = None;
    st.load = Load::Ready;
}

fn fetch(st: &mut State, app: &App, token: String) {
    st.failed.remove(&token);
    if st.pending.insert(token.clone()) {
        app.backend.request(Req::CommentsPage { token });
    }
}

fn open_thread(st: &mut State, app: &App, id: &str, token: String) {
    st.open.insert(id.to_owned());
    if !st.threads.contains_key(id) {
        st.threads.insert(id.to_owned(), vec![Node::More { token: token.clone(), label: String::new(), depth: 1 }]);
        fetch(st, app, token);
    }
}

/// What came back for a `Req::Comments`, `Req::CommentsPage` or `Req::CommentAction`, by
/// its tag (`None`: it failed).
pub fn on_page(tag: &str, page: Option<Value>) {
    let mut guard = STATE.lock().unwrap();
    let st = &mut *guard;
    st.pending.remove(tag);
    if let Some(video) = tag.strip_prefix("video:") {
        if st.video.as_deref() == Some(video) {
            match page {
                None => st.load = Load::Failed,
                Some(v) if v["commentsOff"] == true => st.load = Load::Off,
                Some(v) => fill(st, &v),
            }
        }
        return;
    }
    if st.sorting.as_deref() == Some(tag) {
        st.sorting = None;
        match page {
            Some(v) => fill(st, &v),
            None => st.note = Some("Couldn't sort the comments.".into()),
        }
        return;
    }
    if let Some(rest) = tag.strip_prefix("like:") {
        let (id, _) = rest.rsplit_once(':').unwrap_or((rest, ""));
        if page.is_none() {
            st.liked.remove(id);
            st.note = Some("Couldn't like that comment.".into());
        }
        return;
    }
    if tag == "posted" || tag.starts_with("replied:") {
        st.sending = false;
        let thread = tag.strip_prefix("replied:");
        let Some(v) = page else {
            st.note = Some(if thread.is_some() { "Couldn't post your reply; it's still in the field." } else { "Couldn't post your comment; it's still in the box." }.into());
            return;
        };
        // The new comment comes back under actions[].create…Action.contents.
        let items: Vec<Value> = v["actions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_object)
            .flat_map(|o| o.values())
            .filter(|x| !x["contents"].is_null())
            .map(|x| x["contents"].clone())
            .collect();
        let mut fresh = Vec::new();
        nodes(&items, &mutations(&v), if thread.is_some() { 1 } else { 0 }, &mut fresh);
        if let Some(thread) = thread {
            st.reply_to = None;
            st.reply_draft.clear();
            if let Some(list) = st.threads.get_mut(thread) {
                list.splice(0..0, fresh);
                st.open.insert(thread.to_owned());
            }
            st.note = Some("Reply posted.".into());
        } else {
            st.draft.clear();
            st.root.splice(0..0, fresh);
            st.note = Some("Comment posted.".into());
        }
        return;
    }
    // A "Show more": the page takes its place.
    let Some(v) = page else {
        st.failed.insert(tag.to_owned());
        return;
    };
    let (items, muts) = (continuation_items(&v), mutations(&v));
    let mut authors: Vec<String> = std::iter::once(&st.root)
        .chain(st.threads.values())
        .flatten()
        .filter_map(|n| if let Node::Comment(c, _) = n { Some(c.author.clone()) } else { None })
        .collect();
    for list in std::iter::once(&mut st.root).chain(st.threads.values_mut()) {
        let Some(i) = list.iter().position(|n| matches!(n, Node::More { token, .. } if token == tag)) else { continue };
        let Node::More { depth, .. } = list[i] else { unreachable!() };
        let mut fresh = Vec::new();
        nodes(&items, &muts, depth, &mut fresh);
        authors.extend(fresh.iter().filter_map(|n| if let Node::Comment(c, _) = n { Some(c.author.clone()) } else { None }));
        mark_mentions(&mut fresh, &authors);
        list.splice(i..=i, fresh);
        return;
    }
}

enum Act {
    Toggle(String, Option<String>),
    Fetch(String),
    Sort(bool),
    Reply(String, String),
    CancelReply,
    /// Its params, and the top-level thread it goes under.
    SendReply(Option<String>, String),
    SendComment,
    CancelComment,
    /// The comment, the action to send, and whether it ends up liked.
    Like(String, Option<String>, bool),
    SignIn,
    Retry,
}

/// The panel: the count and Top / Newest, the comment box, then the comments.
pub fn panel(app: &mut App, ui: &mut Ui) {
    let Some(video) = app.playing_id().map(str::to_owned) else { return };
    let mut guard = STATE.lock().unwrap();
    let st = &mut *guard;
    // Another song: its own comments, asked for only now that the panel shows.
    if st.video.as_deref() != Some(video.as_str()) {
        *st = State { video: Some(video.clone()), ..Default::default() };
        app.backend.request(Req::Comments { video_id: video });
    }
    let mut acts = Vec::new();
    match st.load {
        Load::Loading => {
            ui.label(RichText::new("Loading comments…").color(theme::text_dim()));
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
        Load::Off => {
            ui.label(RichText::new("Comments are turned off for this song.").color(theme::text_dim()));
        }
        Load::Failed => {
            ui.label(RichText::new("Couldn't load the comments.").color(theme::text_dim()));
            ui.add_space(4.0);
            if link(ui, ui.cursor().min + vec2(6.0, 4.0), "Try again", None, "comments-retry") {
                acts.push(Act::Retry);
            }
        }
        Load::Ready => list(app, ui, st, &mut acts),
    }
    for a in acts {
        match a {
            Act::Retry => {
                st.load = Load::Loading;
                app.backend.request(Req::Comments { video_id: st.video.clone().unwrap_or_default() });
            }
            Act::Toggle(id, token) => {
                if !st.open.remove(&id)
                    && let Some(token) = token
                {
                    open_thread(st, app, &id, token);
                }
            }
            Act::Fetch(token) => fetch(st, app, token),
            Act::Sort(newest) => {
                if let Some(token) = st.sort[usize::from(newest)].clone()
                    && st.newest != newest
                    && st.sorting.is_none()
                {
                    st.newest = newest;
                    st.sorting = Some(token.clone());
                    app.backend.request(Req::CommentsPage { token });
                }
            }
            Act::Reply(id, prefill) => {
                if st.reply_to.as_deref() != Some(&id) {
                    st.reply_to = Some(id);
                    st.reply_draft = prefill;
                }
            }
            Act::CancelReply => {
                st.reply_to = None;
                st.reply_draft.clear();
            }
            Act::SendReply(params, thread) => match params {
                _ if st.sending => {}
                Some(p) if app.logged_in => {
                    st.sending = true;
                    let body = json!({ "createReplyParams": p, "commentText": st.reply_draft.trim() });
                    app.backend.request(Req::CommentAction { endpoint: "comment/create_comment_reply".into(), body, tag: format!("replied:{thread}") });
                    st.note = Some("Posting your reply…".into());
                }
                _ => st.note = Some("YouTube doesn't allow replies to this comment.".into()),
            },
            Act::SendComment => match st.create_params.clone() {
                _ if st.sending => {}
                Some(p) if app.logged_in => {
                    st.sending = true;
                    let body = json!({ "createCommentParams": p, "commentText": st.draft.trim() });
                    app.backend.request(Req::CommentAction { endpoint: "comment/create_comment".into(), body, tag: "posted".into() });
                    st.note = Some("Posting your comment…".into());
                }
                _ => st.note = Some("YouTube doesn't allow comments here.".into()),
            },
            Act::CancelComment => st.draft.clear(),
            Act::Like(id, action, on) => match action {
                _ if !app.logged_in => st.note = Some("Sign in to like comments.".into()),
                Some(a) => {
                    st.liked.insert(id.clone(), on);
                    let body = json!({ "actions": [a] });
                    app.backend.request(Req::CommentAction { endpoint: "comment/perform_comment_action".into(), body, tag: format!("like:{id}:{}", u8::from(on)) });
                }
                None => st.note = Some("YouTube doesn't allow liking this comment.".into()),
            },
            Act::SignIn => app.start_sign_in(ui.ctx()),
        }
    }
}

/// The loaded list: header, comment box, comments and their open replies.
fn list(app: &App, ui: &mut Ui, st: &mut State, acts: &mut Vec<Act>) {
    let signed_in = app.logged_in;
    let look = app.look();
    let row = look.app.row_look.clone();
    let accent = app.accent_color();
    ui.horizontal(|ui| {
        ui.label(RichText::new(&st.count).font(theme::semibold(15.0)).color(theme::text()));
        if st.sort.iter().all(Option::is_some) {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::default_chip(ui, "Newest", st.newest).clicked() {
                    acts.push(Act::Sort(true));
                }
                if widgets::default_chip(ui, "Top", !st.newest).clicked() {
                    acts.push(Act::Sort(false));
                }
            });
        }
    });
    ui.add_space(10.0);
    egui::ScrollArea::vertical().id_salt(("np-comments", st.video.clone(), st.newest)).auto_shrink(false).show(ui, |ui| {
        composer(ui, st, signed_in, app.account_avatar.as_deref(), &row, acts);
        if st.sorting.is_some() {
            text_at(ui, ui.cursor().min + vec2(row.pad.max(8.0), 4.0), "Loading comments…", theme::regular(13.0), theme::text_dim(), 240.0);
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        if st.root.is_empty() {
            text_at(ui, ui.cursor().min + vec2(row.pad.max(8.0), 4.0), "No comments yet.", theme::regular(13.0), theme::text_dim(), 240.0);
        }
        let root = st.root.clone();
        for (i, n) in root.iter().enumerate() {
            match n {
                Node::Comment(c, d) => {
                    comment_row(ui, st, c, &c.id, *d, i > 0, signed_in, &row, accent, acts);
                    if st.open.contains(&c.id) {
                        for n in st.threads.get(&c.id).cloned().unwrap_or_default() {
                            match &n {
                                Node::Comment(r, d) => comment_row(ui, st, r, &c.id, *d, false, signed_in, &row, accent, acts),
                                Node::More { token, label, depth } => more_row(ui, st, token, label, *depth, &row, acts),
                            }
                        }
                    }
                }
                Node::More { token, label, depth } => more_row(ui, st, token, label, *depth, &row, acts),
            }
        }
        ui.add_space(24.0);
    });
}

/// A rounded text field in `rect`.
fn field(ui: &mut Ui, rect: Rect, text: &mut String, hint: &str, salt: &str) -> egui::Response {
    let inner = rect.shrink2(vec2(16.0, 0.0));
    // The fill goes under the text, but how bright it is depends on the field's focus.
    let bg = ui.painter().add(egui::Shape::Noop);
    let edit = ui.put(
        inner,
        egui::TextEdit::singleline(text)
            .id_salt(salt)
            .hint_text(RichText::new(hint).color(theme::text_faint()))
            .font(theme::regular(14.0))
            .text_color(theme::text())
            .frame(egui::Frame::NONE)
            .align(egui::Align2::LEFT_CENTER)
            .desired_width(inner.width()),
    );
    let k = theme::anim_bool(ui.ctx(), edit.id.with("f"), edit.has_focus(), 0.15);
    ui.painter().set(bg, egui::Shape::rect_filled(rect, rect.height() / 2.0, theme::glass(0.10 + 0.04 * k)));
    ui.painter().rect_stroke(rect, rect.height() / 2.0, egui::Stroke::new(1.0, theme::glass(0.10 + 0.30 * k)), egui::StrokeKind::Inside);
    edit
}

/// Pills in a row, right-aligned in `rect`: (label, primary, enabled). Which were clicked.
fn pills(ui: &mut Ui, rect: Rect, list: &[(&str, bool, bool)]) -> Vec<bool> {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::right_to_left(egui::Align::Center)));
    child.spacing_mut().item_spacing.x = 8.0;
    let mut clicked: Vec<bool> = list.iter().rev().map(|&(label, primary, on)| child.add_enabled_ui(on, |ui| widgets::pill(ui, label, None, primary)).inner.clicked()).collect();
    clicked.reverse();
    clicked
}

/// The box at the top: signed in, a field to comment in; signed out, a prompt to sign in.
fn composer(ui: &mut Ui, st: &mut State, signed_in: bool, photo: Option<&str>, row: &crate::preset::RowLook, acts: &mut Vec<Act>) {
    let pad = row.pad.max(8.0);
    let left = ui.max_rect().left() + pad;
    let right = ui.max_rect().right() - pad;
    let top = ui.cursor().top();
    let avatar = Rect::from_min_size(pos2(left, top + 4.0), vec2(32.0, 32.0));
    ui.painter().circle_filled(avatar.center(), 16.0, theme::glass(0.14));
    // Signed in: the account's own picture, as beside YouTube's box.
    if let Some(url) = photo.filter(|_| signed_in) {
        widgets::paint_remote(ui, url, avatar, 16.0, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    let x = avatar.right() + 12.0;
    let mut h = 40.0;
    if !signed_in {
        text_at(ui, pos2(x, top + 11.0), "Sign in to comment", theme::regular(14.0), theme::text(), right - x - 120.0);
        let r = Rect::from_min_max(pos2(x, top + 1.0), pos2(right, top + 39.0));
        if pills(ui, r, &[("Sign in", true, true)])[0] {
            acts.push(Act::SignIn);
        }
    } else if st.create_params.is_some() {
        let f = Rect::from_min_max(pos2(x, top), pos2(right, top + 40.0));
        let edit = field(ui, f, &mut st.draft, "Add a comment…", "comment-box");
        if edit.has_focus() || !st.draft.trim().is_empty() {
            let r = Rect::from_min_max(pos2(x, f.bottom() + 8.0), pos2(right, f.bottom() + 46.0));
            let c = pills(ui, r, &[("Cancel", false, true), ("Comment", true, !st.draft.trim().is_empty() && !st.sending)]);
            if c[0] {
                acts.push(Act::CancelComment);
            }
            if c[1] {
                acts.push(Act::SendComment);
            }
            h += 46.0;
        }
    } else {
        text_at(ui, pos2(x, top + 11.0), "Commenting isn't available here", theme::regular(14.0), theme::text_dim(), right - x);
    }
    if let Some(note) = &st.note {
        text_at(ui, pos2(x, top + h + 8.0), note, theme::regular(12.5), theme::text_dim(), right - x);
        h += 26.0;
    }
    ui.allocate_rect(Rect::from_min_max(pos2(ui.max_rect().left(), top), pos2(ui.max_rect().right(), top + h + 14.0)), Sense::hover());
}

#[allow(clippy::too_many_arguments)]
fn comment_row(ui: &mut Ui, st: &mut State, c: &Comment, thread: &str, depth: u8, separator: bool, signed_in: bool, row: &crate::preset::RowLook, accent: Color32, acts: &mut Vec<Act>) {
    let w = ui.available_width();
    let top = ui.cursor().top();
    let pad = row.pad.max(8.0);
    let indent = INDENT * f32::from(depth.min(2));
    let size = if depth == 0 { 32.0 } else { 24.0 };
    let left = ui.max_rect().left() + indent;
    let avatar = Rect::from_min_size(pos2(left + pad, top + pad), vec2(size, size));
    let x = avatar.right() + 12.0;
    let tw = (ui.max_rect().right() - pad - x).max(60.0);
    let fmt = |font| egui::TextFormat { font_id: font, color: theme::text(), line_height: Some(20.0), ..Default::default() };
    let mut job = egui::text::LayoutJob::default();
    job.append(&c.text[..c.mention], 0.0, fmt(theme::semibold(14.0)));
    job.append(&c.text[c.mention..], 0.0, fmt(theme::regular(14.0)));
    job.wrap.max_width = tw;
    job.wrap.max_rows = 6;
    let body = ui.fonts_mut(|f| f.layout_job(job));
    let toggle = depth == 0 && c.replies_token.is_some() && !c.replies.is_empty() && c.replies != "0";
    let h = pad + 18.0 + 4.0 + body.size().y + 8.0 + 18.0 + if toggle { 30.0 } else { 0.0 } + pad;
    let rect = Rect::from_min_size(pos2(left, top), vec2(w - indent, h));
    let resp = ui.allocate_rect(rect, Sense::hover());
    let hk = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
    if hk > 0.0 {
        ui.painter().rect_filled(rect, row.radius, theme::glass(row.hover * hk));
    }
    if row.separator > 0.0 && separator {
        ui.painter().hline(rect.x_range(), rect.top(), egui::Stroke::new(1.0, theme::glass(row.separator)));
    }
    ui.painter().circle_filled(avatar.center(), size / 2.0, theme::glass(0.12));
    if !c.avatar.is_empty() {
        widgets::paint_remote(ui, &c.avatar, avatar, size / 2.0, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    // @handle · time; the uploader's own handle on a pill, as YouTube marks it.
    let y = top + pad;
    let name_font = theme::semibold(13.0);
    let name_r = if c.creator {
        let g = widgets::galley(ui, &c.author, name_font.clone(), theme::on_fill(theme::text_dim()), tw);
        let pill = Rect::from_min_size(pos2(x, y - 1.0), vec2(g.size().x + 12.0, 20.0));
        ui.painter().rect_filled(pill, 10.0, theme::text_dim());
        ui.painter().galley(pill.min + vec2(6.0, (20.0 - g.size().y) / 2.0), g, Color32::BLACK);
        pill
    } else {
        text_at(ui, pos2(x, y), &c.author, name_font, theme::text(), tw * 0.7)
    };
    text_at(ui, pos2(name_r.right() + 6.0, y + 1.0), &c.published, theme::regular(12.0), theme::text_dim(), tw * 0.3);
    ui.painter().galley(pos2(x, y + 22.0), body.clone(), theme::text());
    // The like: the thumb and count are a button. Liked, the thumb is filled and white;
    // hover only brightens it.
    let ty = y + 22.0 + body.size().y + 8.0;
    let liked = st.liked.get(&c.id).copied().unwrap_or(c.liked);
    let count = if liked && !c.likes_liked.is_empty() { &c.likes_liked } else { &c.likes };
    let cw = widgets::galley(ui, count, theme::regular(12.5), Color32::PLACEHOLDER, 80.0).size().x;
    let hit = Rect::from_min_max(pos2(x - 4.0, ty - 4.0), pos2(x + 22.0 + cw + 8.0, ty + 22.0));
    let like = widgets::named(ui.interact(hit, ui.scope_id().with(("like", &c.id)), Sense::click()), if liked { "Unlike" } else { "Like" }).on_hover_cursor(egui::CursorIcon::PointingHand);
    let lk = theme::anim_bool(ui.ctx(), like.id.with("h"), like.hovered(), 0.15);
    let col = if liked { theme::text() } else { theme::lerp_color(theme::text_dim(), theme::text(), lk) };
    if like.clicked() {
        acts.push(Act::Like(c.id.clone(), if liked { c.unlike_action.clone() } else { c.like_action.clone() }, !liked));
    }
    if liked {
        thumb_fill(ui, pos2(x + 8.0, ty + 9.0), 15.0, col);
    }
    icon_at(ui, pos2(x + 8.0, ty + 9.0), THUMBS_UP, 15.0, col);
    let lr = text_at(ui, pos2(x + 22.0, ty + 1.0), count, theme::regular(12.5), col, 80.0);
    let mut rx = lr.right() + 16.0;
    if c.hearted {
        icon_at(ui, pos2(rx + 7.0, ty + 9.0), theme::icon::HEART, 14.0, accent);
        rx += 26.0;
    }
    if link(ui, pos2(rx, ty + 1.0), "Reply", None, &format!("reply-{}", c.id)) {
        let prefill = if depth > 0 { format!("{} ", c.author) } else { String::new() };
        acts.push(Act::Reply(c.id.clone(), prefill));
    }
    // "▾ 11 replies" / "▴ Hide replies", white: a cover-sampled accent can be the backdrop's own colour.
    if toggle {
        let open = st.open.contains(&c.id);
        let label = if open {
            "Hide replies".to_owned()
        } else {
            format!("{} {}", c.replies, if c.replies == "1" { "reply" } else { "replies" })
        };
        let glyph = if open { theme::icon::CHEVRON_UP } else { theme::icon::CHEVRON_DOWN };
        if link(ui, pos2(x, ty + 30.0), &label, Some(glyph), &format!("replies-{}", c.id)) {
            acts.push(Act::Toggle(c.id.clone(), c.replies_token.clone()));
        }
    }
    if st.reply_to.as_deref() == Some(&c.id) {
        reply_box(ui, st, c, thread, x, signed_in, row, acts);
    }
}

/// The inside of Lucide's thumbs-up, to fill under its outline glyph (the icon font has
/// outlines only). Convex pieces of its 24-unit path: the cuff, the hand, the thumb.
fn thumb_fill(ui: &Ui, center: egui::Pos2, size: f32, color: Color32) {
    // Map the path's units the way `icon_at` places the glyph: its ink centred on `center`.
    let g = ui.painter().layout_no_wrap(THUMBS_UP.to_string(), theme::icon_font(size), color);
    let ink = g.mesh_bounds;
    // The stroked path spans x 1..22.75 and y 1..23 (2..21.75 and 2..22, plus a 1-unit stroke).
    let k = if ink.is_positive() { ink.height() / 22.0 } else { size / 24.0 };
    let p = |x: f32, y: f32| center + vec2((x - 11.875) * k, (y - 12.0) * k);
    let pieces: [&[(f32, f32)]; 3] = [
        &[(2.5, 10.5), (7.0, 10.5), (7.0, 21.5), (2.5, 21.5)],
        &[(7.0, 10.0), (19.83, 10.0), (21.4, 11.0), (21.75, 12.56), (19.42, 20.56), (17.5, 22.0), (7.0, 22.0)],
        &[(7.0, 10.0), (12.0, 2.0), (13.5, 2.3), (14.8, 3.5), (15.0, 5.88), (14.0, 10.0)],
    ];
    for piece in pieces {
        let pts = piece.iter().map(|&(x, y)| p(x, y)).collect();
        ui.painter().add(egui::Shape::convex_polygon(pts, color, egui::Stroke::NONE));
    }
}

/// A clickable bit of text, white and semibold, on a soft pill when hovered. `true`: clicked.
fn link(ui: &mut Ui, at: egui::Pos2, label: &str, glyph: Option<char>, salt: &str) -> bool {
    let gw = if glyph.is_some() { 20.0 } else { 0.0 };
    let g = widgets::galley(ui, label, theme::semibold(12.5), Color32::PLACEHOLDER, 240.0);
    let r = Rect::from_min_size(at - vec2(6.0, 4.0), vec2(g.size().x + gw + 12.0, g.size().y + 8.0));
    let resp = widgets::named(ui.interact(r, ui.scope_id().with(salt), Sense::click()), label).on_hover_cursor(egui::CursorIcon::PointingHand);
    let k = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
    if k > 0.0 {
        ui.painter().rect_filled(r, r.height() / 2.0, theme::glass(0.10 * k));
    }
    if let Some(gl) = glyph {
        icon_at(ui, pos2(at.x + 7.0, at.y + g.size().y / 2.0), gl, 14.0, theme::text());
    }
    ui.painter().galley(pos2(at.x + gw, at.y), g, theme::text());
    resp.clicked()
}

/// Under the comment being answered: signed in, a field and Cancel / Reply; signed out, a
/// prompt to sign in.
#[allow(clippy::too_many_arguments)]
fn reply_box(ui: &mut Ui, st: &mut State, c: &Comment, thread: &str, x: f32, signed_in: bool, row: &crate::preset::RowLook, acts: &mut Vec<Act>) {
    let right = ui.max_rect().right() - row.pad.max(8.0);
    let top = ui.cursor().top();
    let h = if signed_in {
        let f = Rect::from_min_max(pos2(x, top), pos2(right, top + 38.0));
        field(ui, f, &mut st.reply_draft, "Add a reply…", &format!("reply-box-{}", c.id));
        let r = Rect::from_min_max(pos2(x, f.bottom() + 8.0), pos2(right, f.bottom() + 46.0));
        let b = pills(ui, r, &[("Cancel", false, true), ("Reply", true, !st.reply_draft.trim().is_empty() && !st.sending)]);
        if b[0] {
            acts.push(Act::CancelReply);
        }
        if b[1] {
            acts.push(Act::SendReply(c.reply_params.clone(), thread.to_owned()));
        }
        38.0 + 46.0
    } else {
        text_at(ui, pos2(x, top + 11.0), "Sign in to reply", theme::regular(14.0), theme::text(), right - x - 200.0);
        let r = Rect::from_min_max(pos2(x, top + 1.0), pos2(right, top + 39.0));
        let b = pills(ui, r, &[("Cancel", false, true), ("Sign in", true, true)]);
        if b[0] {
            acts.push(Act::CancelReply);
        }
        if b[1] {
            acts.push(Act::SignIn);
        }
        40.0
    };
    ui.allocate_rect(Rect::from_min_max(pos2(x, top), pos2(right, top + h + 12.0)), Sense::hover());
}

/// "Show more replies", "Loading replies…", or a retry when it failed.
fn more_row(ui: &mut Ui, st: &State, token: &str, label: &str, depth: u8, row: &crate::preset::RowLook, acts: &mut Vec<Act>) {
    let pad = row.pad.max(8.0);
    let x = ui.max_rect().left() + INDENT * f32::from(depth.min(2)) + pad;
    let top = ui.cursor().top();
    if st.pending.contains(token) {
        let what = if depth == 0 { "Loading comments…" } else { "Loading replies…" };
        text_at(ui, pos2(x, top + 8.0), what, theme::regular(12.5), theme::text_dim(), 240.0);
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    } else if st.failed.contains(token) {
        if link(ui, pos2(x, top + 8.0), "Couldn't load these. Try again", None, &format!("retry-{token}")) {
            acts.push(Act::Fetch(token.to_owned()));
        }
    } else if link(ui, pos2(x, top + 8.0), if label.is_empty() { "Show more replies" } else { label }, Some(theme::icon::CHEVRON_DOWN), &format!("more-{token}")) {
        acts.push(Act::Fetch(token.to_owned()));
    }
    ui.allocate_rect(Rect::from_min_size(pos2(x, top), vec2(200.0, 34.0)), Sense::hover());
}
