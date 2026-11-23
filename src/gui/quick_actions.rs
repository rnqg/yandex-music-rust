use super::*;
use winit::keyboard::{KeyCode, PhysicalKey};

enum Action {
    Wave,
    Collection,
    Mini,
    Track(Box<Track>),
    Playlist(String, String),
    Artist(String, String),
}

#[derive(Default)]
pub(super) struct QuickState {
    query: String,
    timer: Timer,
    actions: Vec<Action>,
    finished: bool,
}

impl Ctx {
    pub(super) fn quick_toggle(self: &Rc<Self>) {
        let ui = self.ui();
        let app = ui.global::<App>();
        if app.get_quick_open() {
            app.set_quick_open(false);
            self.st.borrow_mut().quick = QuickState::default();
            app.set_quick_items(ModelRc::default());
            return;
        }
        if app.get_logged_out() || app.get_mini() || app.get_about_open() || app.get_wave_fact_open()
            || app.get_wave_filters_open() || app.get_wave_rooms_open() { return; }
        app.set_menu_open(false);
        app.set_account_open(false);
        app.set_eq_open(false);
        app.set_quick_text("".into());
        app.set_quick_open(true);
        self.quick_edited("");
    }

    pub(super) fn quick_edited(self: &Rc<Self>, text: &str) {
        let ui = self.ui();
        let app = ui.global::<App>();
        let query = text.trim().to_owned();
        app.set_quick_index(0);
        app.set_quick_items(ModelRc::default());
        app.set_quick_status("Поиск…".into());
        let mut st = self.st.borrow_mut();
        st.quick.timer.stop();
        st.quick.query = query;
        st.quick.finished = false;
        st.quick.actions.clear();
        if !st.quick.query.is_empty() {
            st.quick.timer.start(TimerMode::SingleShot, Duration::from_millis(220), || with_ctx(|ctx| ctx.refresh_quick()));
        } else {
            drop(st);
            self.refresh_quick();
        }
    }

    pub(super) fn refresh_quick(&self) {
        let ui = self.ui();
        let app = ui.global::<App>();
        if !app.get_quick_open() { return; }
        let mut st = self.st.borrow_mut();
        if st.quick.finished || st.quick.timer.running() { return; }
        let icons = ui.global::<Icons>();
        let mut rows = Vec::new();
        let mut actions = Vec::new();
        let mut add = |action: Action, text: String, icon: Image| {
            rows.push(MenuEntry { text: text.into(), icon, submenu: matches!(action, Action::Track(_)), ..Default::default() });
            actions.push(action);
        };
        if st.quick.query.is_empty() {
            add(Action::Wave, "Включить Мою волну".into(), icons.get_m_vibe());
            add(Action::Collection, "Открыть коллекцию".into(), icons.get_nav_collection());
            add(Action::Mini, "Мини-плеер".into(), icons.get_mini_player());
        } else {
            let Some(net) = st.net.as_ref() else { return };
            let Some(result) = net.get(&format!("search:all:{}", st.quick.query)) else { return };
            let data = match result {
                Ok(data) => data,
                Err(_) => {
                    st.quick.finished = true;
                    app.set_quick_status("Не удалось загрузить результаты".into());
                    return;
                }
            };
            let Data::Search(results) = data.as_ref() else { return };
            for t in results.tracks.iter().take(4) {
                add(Action::Track(Box::new(t.clone())), format!("{} · {}", t.title, t.artists), icons.get_play_big());
            }
            for p in results.playlists.iter().take(2) {
                add(Action::Playlist(format!("playlist:{}:{}", p.uid, p.kind), p.title.clone()), p.title.clone(), icons.get_m_add_playlist());
            }
            for a in results.artists.iter().take(2) {
                add(Action::Artist(a.id.clone(), a.name.clone()), format!("Волна · {}", a.name), icons.get_m_vibe());
            }
        }
        st.quick.actions = actions;
        st.quick.finished = true;
        app.set_quick_status("Ничего не найдено".into());
        app.set_quick_items(ModelRc::from(Rc::new(VecModel::from(rows))));
    }

    pub(super) fn quick_pick(self: &Rc<Self>, index: i32, enqueue: bool) {
        let Ok(index) = usize::try_from(index) else { return };
        let mut st = self.st.borrow_mut();
        if index >= st.quick.actions.len() { return; }
        let action = st.quick.actions.remove(index);
        drop(st);
        self.quick_toggle();
        match action {
            Action::Wave => {
                self.navigate(Page::Wave, None, true);
                self.player.send(Cmd::Wave(Source::Wave));
                self.start();
            }
            Action::Collection => self.navigate(Page::Collection, None, true),
            Action::Mini => self.ui().global::<App>().invoke_toggle_mini(),
            Action::Track(t) if enqueue => {
                self.player.send(Cmd::Enqueue { track: *t, next: true });
                self.toast("Трек будет играть следующим");
            }
            Action::Track(t) => {
                if self.player.state().track.as_ref().is_some_and(|c| c.id == t.id) {
                    self.start();
                } else {
                    self.play_tracks(vec![*t], 0, ListKind::Tracks, "search", "Поиск");
                }
            }
            Action::Playlist(target, title) => self.open_target(&target, &title),
            Action::Artist(id, title) => {
                self.player.send(Cmd::Wave(Source::Seed { station: format!("artist:{id}"), header: "Моя волна по исполнителю".into(), title }));
                self.start();
            }
        }
    }

    pub(super) fn quick_key(self: &Rc<Self>, key: PhysicalKey, repeat: bool, mods: winit::keyboard::ModifiersState) -> bool {
        let ui = self.ui();
        let app = ui.global::<App>();
        if app.get_macos() && mods.super_key() && key == PhysicalKey::Code(KeyCode::KeyK) {
            if !repeat { self.quick_toggle(); }
            return true;
        }
        if !app.get_quick_open() { return false; }
        match key {
            PhysicalKey::Code(KeyCode::Escape) => self.quick_toggle(),
            PhysicalKey::Code(KeyCode::ArrowUp | KeyCode::ArrowDown) => {
                let d = if key == PhysicalKey::Code(KeyCode::ArrowUp) { -1 } else { 1 };
                let len = app.get_quick_items().row_count() as i32;
                if len > 0 { app.set_quick_index((app.get_quick_index() + d).rem_euclid(len)); }
            }
            PhysicalKey::Code(KeyCode::Enter | KeyCode::NumpadEnter) => {
                if !repeat { self.quick_pick(app.get_quick_index(), mods.alt_key()); }
            }
            _ => return false,
        }
        true
    }
}
