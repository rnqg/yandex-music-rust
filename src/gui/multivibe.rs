use super::{with_ctx, App, Ctx, Page, WaveRoomItem};
use crate::api::{self, Api, Res, WaveRoom};
use crate::player::{Cmd, Source};
use crate::sys;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::rc::Rc;

enum Request {
    List,
    Create,
    Join(String),
    Rename(String, String),
    Leave(String),
}

enum Change {
    List(Vec<WaveRoom>),
    Put(WaveRoom, bool),
    Rename(String, String),
    Leave(String),
}

impl Request {
    fn execute(self, api: &Api) -> Res<Change> {
        match self {
            Self::List => api.wave_rooms().map(Change::List),
            Self::Create => api.create_wave_room().map(|room| Change::Put(room, true)),
            Self::Join(id) => api.join_wave_room(&id).map(|room| Change::Put(room, false)),
            Self::Rename(id, name) => api.rename_wave_room(&id, &name).map(|_| Change::Rename(id, name)),
            Self::Leave(id) => api.leave_wave_room(&id).map(|_| Change::Leave(id)),
        }
    }

    fn error_message(&self) -> &'static str {
        match self {
            Self::List => "Не удалось загрузить совместные волны",
            Self::Create => "Не удалось создать ссылку",
            Self::Join(_) => "Не удалось присоединиться к волне",
            Self::Rename(..) => "Не удалось переименовать волну",
            Self::Leave(_) => "Не удалось выйти из волны",
        }
    }
}

pub(super) fn register(app: &App) {
    app.on_open_wave_rooms(|| with_ctx(|ctx| ctx.open_wave_rooms()));
    app.on_wave_room_action(|action, id, value| {
        with_ctx(|ctx| ctx.wave_room_action(action.as_str(), id.as_str(), value.as_str()));
    });
}

fn title(room: &WaveRoom) -> String {
    if room.name.trim().is_empty() {
        crate::i18n::tr("Моя волна на двоих")
    } else {
        room.name.clone()
    }
}

impl Ctx {
    pub(super) fn open_wave_rooms(&self) {
        if self.st.borrow().net.is_none() {
            return;
        }
        let ui = self.ui();
        let app = ui.global::<App>();
        app.set_wave_rooms_open(true);
        self.request_rooms(Request::List);
    }

    fn wave_room_action(self: &Rc<Self>, action: &str, id: &str, value: &str) {
        let ui = self.ui();
        let app = ui.global::<App>();
        if !app.get_wave_rooms_open() || app.get_wave_rooms_busy() {
            return;
        }
        let request = match action {
            "refresh" => Request::List,
            "create" => Request::Create,
            "join" => {
                let Some(id) = api::wave_room_invitation(value) else {
                    app.set_wave_rooms_error(crate::i18n::tr("Вставьте ссылку-приглашение из Яндекс Музыки").into());
                    return;
                };
                Request::Join(id)
            }
            _ => {
                let st = self.st.borrow();
                let Some(room) = st.wave_rooms.iter().find(|room| room.id == id) else { return };
                match action {
                    "copy" => {
                        let url = room.invitation();
                        drop(st);
                        sys::copy_text(&url);
                        self.toast("Ссылка скопирована");
                        return;
                    }
                    "play" if room.playable() => {
                        let source = Source::SharedWave { id: id.to_owned(), seeds: room.seeds.clone(), title: title(room) };
                        drop(st);
                        let state = self.player.state();
                        if !matches!(&state.source, Source::SharedWave { id: current, .. } if current == id) {
                            self.player.send(Cmd::Wave(source));
                        }
                        app.set_wave_rooms_open(false);
                        self.navigate(Page::Wave, None, true);
                        self.start();
                        return;
                    }
                    "rename" if !value.trim().is_empty() && value.trim() != room.name => {
                        Request::Rename(id.to_owned(), value.trim().to_owned())
                    }
                    "leave" => Request::Leave(id.to_owned()),
                    _ => return,
                }
            }
        };
        self.request_rooms(request);
    }

    fn request_rooms(&self, request: Request) {
        let ui = self.ui();
        let app = ui.global::<App>();
        if app.get_wave_rooms_busy() {
            return;
        }
        let (api, generation) = {
            let mut st = self.st.borrow_mut();
            let Some(api) = st.net.as_ref().map(|net| net.api.clone()) else { return };
            st.wave_room_request += 1;
            (api, st.wave_room_request)
        };
        app.set_wave_rooms_busy(true);
        app.set_wave_rooms_error(SharedString::new());
        std::thread::spawn(move || {
            let message = request.error_message();
            let result = request.execute(&api);
            let _ = slint::invoke_from_event_loop(move || with_ctx(|ctx| {
                if ctx.st.borrow().wave_room_request != generation {
                    return;
                }
                let ui = ctx.ui();
                let app = ui.global::<App>();
                app.set_wave_rooms_busy(false);
                match result {
                    Ok(change) => ctx.update_wave_rooms(change),
                    Err(_) => app.set_wave_rooms_error(crate::i18n::tr(message).into()),
                }
            }));
        });
    }

    fn update_wave_rooms(self: &Rc<Self>, change: Change) {
        let ui = self.ui();
        let app = ui.global::<App>();
        let mut invitation = None;
        let mut st = self.st.borrow_mut();
        match change {
            Change::List(rooms) => st.wave_rooms = rooms,
            Change::Put(room, copy) => {
                if copy && app.get_wave_rooms_open() {
                    invitation = Some(room.invitation());
                }
                if let Some(old) = st.wave_rooms.iter_mut().find(|old| old.id == room.id) {
                    *old = room;
                } else {
                    st.wave_rooms.insert(0, room);
                }
            }
            Change::Rename(id, name) => {
                if let Some(room) = st.wave_rooms.iter_mut().find(|room| room.id == id) {
                    room.name = name;
                }
            }
            Change::Leave(id) => {
                st.wave_rooms.retain(|room| room.id != id);
                self.player.send(Cmd::WaveRoomLeft(id));
            }
        }
        let rows: Vec<_> = st.wave_rooms.iter().map(|room| WaveRoomItem {
            id: room.id.clone().into(),
            title: title(room).into(),
            members: room.members.clone().into(),
            status: if room.status == "DRAFT" {
                crate::i18n::tr("Ожидает участника").into()
            } else if !room.playable() {
                crate::i18n::tr("Волна недоступна").into()
            } else {
                SharedString::new()
            },
            invite: room.status == "DRAFT",
            playable: room.playable(),
        }).collect();
        drop(st);
        app.set_wave_rooms(ModelRc::from(Rc::new(VecModel::from(rows))));
        if let Some(url) = invitation {
            sys::copy_text(&url);
            self.toast("Ссылка скопирована");
        }
    }
}
