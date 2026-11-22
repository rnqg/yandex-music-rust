//! Неофициальный API Яндекс Музыки.

use base64::Engine as _;
use hmac::{Hmac, Mac};
use md5::{Digest, Md5};
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::Method;
use serde_json::{json, Value};
use sha2::Sha256;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::client::{token_allowed, ANDROID_CLIENT, API_BASE as BASE, MP3_SALT, SIGN_KEY};

pub const WAVE: &str = "user:onyourwave";

pub type Res<T> = Result<T, String>;

pub enum AccountError {
    Unauthorized,
    Unavailable(String),
}

/// Качество звука из настроек: 0 превосходное, 1 оптимальное, 2 экономичное.
pub static QUALITY: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Ссылка на картинку Яндекса: `uri` содержит `%%` вместо размера.
pub fn pic(uri: &str, size: u32) -> String {
    let uri = uri.trim_start_matches("https://").trim_start_matches("http://");
    // get-music-misc отдаёт только эти размеры, на остальные — 404.
    let size = if uri.contains("/get-music-misc/") {
        [30, 40, 50, 75, 80, 100, 150, 160, 200, 300, 400, 460, 600, 700, 800, 1000].into_iter().find(|&s| s >= size).unwrap_or(1000)
    } else {
        [50, 100, 200, 300, 400, 600, 1000].into_iter().find(|&s| s >= size).unwrap_or(1000)
    };
    format!("https://{}", uri.replace("%%", &format!("{size}x{size}")))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Named {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug)]
struct PlaylistSlot {
    revision: u64,
    index: usize,
    album: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Track {
    /// Ревизия и серверная позиция строки, выбранной пользователем.
    playlist_slot: Option<Box<PlaylistSlot>>,
    pub id: String,
    pub album_id: Option<String>,
    pub title: String,
    /// «Hardtekk», «Remix» — официальное приложение пишет её серым после названия.
    pub version: String,
    pub artists: String,
    pub artist_list: Vec<Named>,
    pub duration_ms: u64,
    pub cover: Option<String>,
    pub wave_cutout: Option<String>,
    pub explicit: bool,
    pub lyrics: bool,
    pub color: Option<[u8; 3]>,
    /// Для тегов скачанного файла: альбом, год, номер трека.
    pub album_title: String,
    pub year: Option<u32>,
    pub position: Option<u32>,
    /// Пакет волны, из которого пришёл трек: с ним уходит фидбек по этому треку.
    pub batch_id: String,
}

impl Track {
    /// "trackId:albumId" — такой формат ждут лайки и фидбек волны.
    pub fn full_id(&self) -> String {
        match &self.album_id {
            Some(a) => format!("{}:{}", self.id, a),
            None => self.id.clone(),
        }
    }

    pub fn cover_url(&self, size: u32) -> Option<String> {
        self.cover.as_deref().map(|c| pic(c, size))
    }
}

#[derive(Clone, Debug)]
pub struct Artist {
    pub id: String,
    pub name: String,
    pub cover: Option<String>,
    pub color: Option<[u8; 3]>,
}

#[derive(Clone, Debug)]
pub struct Album {
    pub id: String,
    pub title: String,
    pub artists: String,
    pub year: Option<u64>,
    pub kind: String,
    pub cover: Option<String>,
    pub likes: u64,
    pub label: String,
    pub artist_cover: Option<String>,
    pub artist_list: Vec<Named>,
}

#[derive(Clone, Debug)]
pub struct Playlist {
    pub uid: String,
    pub kind: String,
    pub title: String,
    pub owner: String,
    pub covers: Vec<String>,
    pub description: String,
    /// "2026-10-11T09:24:09+00:00"
    pub modified: String,
    pub color: Option<[u8; 3]>,
    pub likes: u64,
    /// Логин владельца — им адресуется плейлист в коде для вставки.
    pub login: String,
}

#[derive(Clone, Debug)]
pub struct TopArtist {
    pub artist: Artist,
    pub seconds: u64,
    pub position: u32,
    /// same, up, down, new
    pub progress: String,
}

/// Блоки страницы «Коллекция» — те же источники, что у официального приложения.
#[derive(Clone, Debug, Default)]
pub struct Collection {
    pub likes_count: u64,
    pub likes_cover: Option<String>,
    pub likes_preview: Vec<Track>,
    pub top_artists: Vec<TopArtist>,
    pub playlist_tabs: Vec<(String, Vec<Playlist>)>,
    pub album_tabs: Vec<(String, Vec<Album>)>,
}

/// Вайб на колесе «Моей волны».
#[derive(Clone, Debug)]
pub struct WheelItem {
    pub name: String,
    pub description: String,
    pub cover: Option<String>,
    pub round: bool,
    pub action: WheelAction,
}

#[derive(Clone, Debug)]
pub enum WheelAction {
    Play(Vec<String>),
    Album(String),
    Link(String),
    SharedWave,
    Settings,
}

pub struct WaveRoom {
    pub id: String,
    pub name: String,
    pub members: String,
    pub status: String,
    pub seeds: Vec<String>,
}

impl WaveRoom {
    pub fn playable(&self) -> bool {
        self.status == "ENABLED" && !self.seeds.is_empty()
    }

    pub fn invitation(&self) -> String {
        format!("https://music.yandex.ru/multivibe/{}", self.id)
    }
}

pub struct WaveSetting {
    pub title: String,
    pub options: Vec<(String, String)>,
    pub selected: Option<usize>,
    context: bool,
}

#[derive(Default)]
pub struct WaveFilters {
    seeds: Vec<String>,
    pub groups: Vec<WaveSetting>,
}

impl WaveFilters {
    pub fn selected_seeds(&self) -> Vec<String> {
        let context = self.groups.iter().find(|group| group.context);
        let new_context = context.and_then(|group| group.selected.and_then(|index| group.options.get(index)));
        let old_context = context.and_then(|group| group.options.iter().find(|(_, seed)| self.seeds.contains(seed)));
        let context_changed = old_context != new_context;
        let mut seeds = self.seeds.iter().filter(|seed| {
            if context_changed && !seed.starts_with("setting") {
                return false;
            }
            !self.groups.iter().filter(|group| !group.context).any(|group| {
                group.options.iter().any(|(_, option)| option.split_once(':').map(|(key, _)| key)
                    == seed.split_once(':').map(|(key, _)| key))
            })
        }).cloned().collect::<Vec<_>>();
        seeds.extend(self.groups.iter().filter(|group| !group.context).filter_map(|group| {
            group.selected.and_then(|index| group.options.get(index)).map(|(_, option)| option.clone())
        }));
        if !seeds.iter().any(|seed| !seed.starts_with("setting")) {
            seeds.insert(0, new_context.map_or_else(|| WAVE.to_owned(), |(_, seed)| seed.clone()));
        }
        seeds
    }

    pub fn context_title(&self) -> Option<&str> {
        let group = self.groups.iter().find(|group| group.context)?;
        group.options.get(group.selected?).map(|(title, _)| title.as_str())
    }
}

pub fn personal_wave_seeds(seeds: &[String]) -> bool {
    seeds.first().is_some_and(|seed| seed == WAVE) && seeds[1..].iter().all(|seed| seed.starts_with("setting"))
}

#[derive(Clone, Debug)]
pub struct Pin {
    pub title: String,
    pub subtitle: String,
    pub cover: Option<String>,
    pub round: bool,
    /// album:ID, playlist:UID:KIND, artist:ID, station:STATION
    pub target: String,
}

impl Playlist {
    pub fn seed(&self) -> String {
        format!("playlist:{}_{}", self.uid, self.kind)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Search {
    pub tracks: Vec<Track>,
    pub artists: Vec<Artist>,
    pub albums: Vec<Album>,
    pub playlists: Vec<Playlist>,
}

/// Строка синхронного текста: с какой секунды и до какой (до метки следующей строки).
#[derive(Clone, Debug, PartialEq)]
pub struct SyncLine {
    pub from: f32,
    pub to: Option<f32>,
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct Lyrics {
    /// Пустой текст — проигрыш: строка есть, но не видна.
    pub synced: Vec<SyncLine>,
    /// Правообладатель текста («Источник: …»).
    pub source: String,
    pub text: Vec<String>,
    pub writers: String,
}

/// Откуда брать следующий пакет волны.
pub enum WaveSession {
    New,
    Next(String),
    Clone(String),
}

pub struct Batch {
    pub tracks: Vec<Track>,
    pub liked: Vec<String>,
    pub batch_id: String,
    /// Сессия ротора, в которой получены треки (пусто — старый API станций).
    pub session: String,
    pub seeds: Option<Vec<String>>,
}

#[derive(Clone, Debug)]
pub struct Account {
    pub uid: String,
    pub login: String,
    pub has_plus: bool,
}

#[derive(Clone)]
pub struct Api {
    http: Client,
    token: String,
}

fn s(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

pub(crate) fn str_of(v: &Value, key: &str) -> String {
    s(v.get(key)).unwrap_or_default()
}

pub(crate) fn hex_color(v: Option<&Value>) -> Option<[u8; 3]> {
    let h = v?.as_str()?.trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(h, 16).ok()?;
    Some([(n >> 16) as u8, (n >> 8) as u8, n as u8])
}

fn artists_of(v: &Value) -> Vec<Named> {
    v.get("artists")
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    Some(Named {
                        id: s(x.get("id")).unwrap_or_default(),
                        name: x.get("name")?.as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn join_names(a: &[Named]) -> String {
    a.iter().map(|n| n.name.as_str()).collect::<Vec<_>>().join(", ")
}

fn playlist_entry_track(item: &Value, mut track: Track, revision: Option<u64>, index: usize) -> Track {
    track.playlist_slot = revision.map(|revision| Box::new(PlaylistSlot { revision, index, album: s(item.get("albumId")) }));
    track
}

fn playlist_remove_index(list: &Value, track: &Track) -> Res<usize> {
    let items = list.get("tracks").and_then(Value::as_array).ok_or("нет списка треков")?;
    let slot = track.playlist_slot.as_ref().ok_or("Нет ревизии показанного плейлиста. Обновите список")?;
    if list.get("revision").and_then(Value::as_u64) != Some(slot.revision) {
        return Err("Плейлист изменился. Выберите трек ещё раз после обновления".into());
    }
    let item = items.get(slot.index).ok_or("трек уже отсутствует в плейлисте")?;
    let id = s(item.get("id")).or_else(|| s(item.pointer("/track/id")));
    if id.as_ref() != Some(&track.id) || s(item.get("albumId")) != slot.album {
        return Err("Запись плейлиста изменилась. Обновите список".into());
    }
    Ok(slot.index)
}

#[cfg(test)]
#[test]
fn removal_uses_unfiltered_server_order() {
    let track = parse_track(&json!({"id": 7, "title": "A"})).unwrap();
    let list = json!({"revision":12,"tracks":[{"id":1,"available":false},{"id":7,"albumId":3},{"id":7,"albumId":3}]});
    let selected = playlist_entry_track(&list["tracks"][2], track.clone(), Some(12), 2);
    assert_eq!(playlist_remove_index(&list, &selected), Ok(2));
    let mut changed = list.clone();
    changed["revision"] = json!(13);
    assert!(playlist_remove_index(&changed, &selected).is_err());
    assert!(playlist_remove_index(&list, &track).is_err());

}

fn wave_cutout_url(v: &Value) -> Option<String> {
    ["/cutoutCover/uri", "/artists/0/cutoutCover/uri"]
        .into_iter()
        .filter_map(|path| v.pointer(path).and_then(Value::as_str))
        .find_map(|uri| {
            let uri = uri.trim().strip_prefix("//").unwrap_or(uri.trim());
            let url = if uri.contains("://") { uri.to_owned() } else { format!("https://{uri}") };
            let url = reqwest::Url::parse(&url.replace("%%", "orig")).ok()?;
            (url.scheme() == "https"
                && matches!(url.host_str(), Some("avatars.yandex.net" | "avatars.mds.yandex.net" | "avatars.yandex.ru"))
                && url.username().is_empty()
                && url.password().is_none()
                && url.port().is_none())
            .then(|| url.to_string())
        })
}

pub(crate) fn parse_track(v: &Value) -> Option<Track> {
    if v.get("available").and_then(|x| x.as_bool()) == Some(false) {
        return None;
    }
    let id = s(v.get("id"))?;
    let title = v.get("title")?.as_str()?.to_string();
    let version = str_of(v, "version");
    let artist_list = artists_of(v);
    let album = v.get("albums").and_then(|a| a.as_array()).and_then(|a| a.first());
    let li = v.get("lyricsInfo");
    let flag = |k: &str| li.and_then(|l| l.get(k)).and_then(|x| x.as_bool()).unwrap_or(false);
    Some(Track {
        playlist_slot: None,
        id,
        album_id: album.and_then(|a| s(a.get("id"))),
        title,
        version,
        artists: join_names(&artist_list),
        artist_list,
        duration_ms: v.get("durationMs").and_then(|d| d.as_u64()).unwrap_or(0),
        cover: v.get("coverUri").and_then(|c| c.as_str()).map(String::from),
        wave_cutout: wave_cutout_url(v),
        explicit: v.get("contentWarning").and_then(|c| c.as_str()) == Some("explicit")
            || v.get("explicit").and_then(|c| c.as_bool()).unwrap_or(false),
        lyrics: flag("hasAvailableSyncLyrics") || flag("hasAvailableTextLyrics"),
        color: hex_color(v.pointer("/derivedColors/average")),
        album_title: album.map(|a| str_of(a, "title")).unwrap_or_default(),
        year: album.and_then(|a| a.get("year")).and_then(|y| y.as_u64()).map(|y| y as u32),
        position: album.and_then(|a| a.pointer("/trackPosition/index")).and_then(|y| y.as_u64()).map(|y| y as u32),
        batch_id: String::new(),
    })
}

pub(crate) fn parse_artist(v: &Value) -> Option<Artist> {
    Some(Artist {
        id: s(v.get("id"))?,
        name: v.get("name")?.as_str()?.to_string(),
        cover: v
            .pointer("/cover/uri")
            .or_else(|| v.get("ogImage"))
            .and_then(|u| u.as_str())
            .map(String::from),
        color: hex_color(v.pointer("/derivedColors/average")),
    })
}

pub(crate) fn parse_album(v: &Value) -> Option<Album> {
    let artist_list = artists_of(v);
    Some(Album {
        id: s(v.get("id"))?,
        title: v.get("title")?.as_str()?.to_string(),
        artists: join_names(&artist_list),
        artist_list,
        year: v
            .get("year")
            .and_then(|y| y.as_u64())
            .or_else(|| v.get("releaseDate").and_then(|d| d.as_str()).and_then(|d| d.get(..4)).and_then(|y| y.parse().ok())),
        kind: s(v.get("type")).or_else(|| s(v.get("albumType"))).unwrap_or_default(),
        cover: v
            .get("coverUri")
            .or_else(|| v.pointer("/cover/uri"))
            .and_then(|c| c.as_str())
            .map(String::from),
        likes: v.get("likesCount").and_then(|c| c.as_u64()).unwrap_or(0),
        label: v.pointer("/labels/0/name").and_then(|l| l.as_str()).unwrap_or("").to_string(),
        artist_cover: v.pointer("/artists/0/cover/uri").and_then(|u| u.as_str()).map(String::from),
    })
}

pub(crate) fn parse_playlist(v: &Value) -> Option<Playlist> {
    let cover = v.get("cover");
    let mut covers: Vec<String> = Vec::new();
    if let Some(c) = cover {
        if let Some(u) = c.get("uri").and_then(|u| u.as_str()) {
            covers.push(u.to_string());
        } else if let Some(items) = c.get("itemsUri").and_then(|i| i.as_array()) {
            covers.extend(items.iter().filter_map(|i| i.as_str()).take(4).map(String::from));
        }
    }
    if covers.is_empty() {
        if let Some(og) = v.get("ogImage").and_then(|o| o.as_str()) {
            covers.push(og.to_string());
        }
    }
    let owner = v.get("owner");
    Some(Playlist {
        uid: s(v.get("uid")).or_else(|| s(owner?.get("uid")))?,
        kind: s(v.get("kind"))?,
        title: str_of(v, "title"),
        owner: owner.map(|o| str_of(o, "name")).filter(|n| !n.is_empty()).or_else(|| owner.map(|o| str_of(o, "login"))).unwrap_or_default(),
        covers,
        description: str_of(v, "description"),
        modified: str_of(v, "modified"),
        color: hex_color(v.pointer("/derivedColors/average")),
        likes: v.get("likesCount").and_then(|c| c.as_u64()).unwrap_or(0),
        login: owner.map(|o| str_of(o, "login")).unwrap_or_default(),
    })
}

static LANG: std::sync::RwLock<String> = std::sync::RwLock::new(String::new());

/// «Меньше ИИ»: оригинал шлёт его заголовком в запросах сессий ротора.
pub static LESS_AI: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// «Треки 18+»: выключено — новая сессия волны просит без них (`allowExplicit`), а плеер
/// пропускает треки с пометкой E, как оригинал с фильтром очереди.
pub static ALLOW_EXPLICIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn allow_explicit() -> bool {
    ALLOW_EXPLICIT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Время для фидбека сессий — ISO 8601 в UTC, как `new Date().toISOString()`.
pub fn iso_now() -> String {
    let d = now();
    let secs = d.as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Дата из числа дней с 1970-01-01 (алгоритм civil_from_days).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        d.subsec_millis()
    )
}

/// Фидбек сессии ротора: radioStarted / trackStarted / trackFinished / skip.
pub fn session_event(
    kind: &str,
    track: Option<&str>,
    played: Option<f64>,
    length: Option<f64>,
    batch_id: &str,
    from: &str,
) -> Value {
    let mut event = json!({ "type": kind, "timestamp": iso_now() });
    if kind == "radioStarted" {
        event["from"] = json!(from);
    }
    if let Some(t) = track {
        event["trackId"] = json!(t);
    }
    if let Some(p) = played {
        event["totalPlayedSeconds"] = json!(p);
    }
    if let Some(l) = length {
        event["trackLengthSeconds"] = json!(l);
    }
    let mut body = json!({ "event": event, "from": from });
    if !batch_id.is_empty() && kind != "radioStarted" {
        body["batchId"] = json!(batch_id);
    }
    body
}

fn parse_batch(v: &Value, session: String) -> Res<Batch> {
    let batch_id = str_of(v, "batchId");
    let mut tracks = Vec::new();
    let mut liked = Vec::new();
    for it in v.get("sequence").and_then(|s| s.as_array()).into_iter().flatten() {
        if let Some(mut t) = it.get("track").and_then(parse_track) {
            t.batch_id = batch_id.clone();
            if it.get("liked").and_then(|x| x.as_bool()) == Some(true) {
                liked.push(t.id.clone());
            }
            tracks.push(t);
        }
    }
    if tracks.is_empty() {
        return Err("волна вернула пустой список".into());
    }
    let seeds = wave_seeds(v.get("wave"));
    Ok(Batch { tracks, liked, batch_id, session, seeds })
}

fn wave_seeds(data: Option<&Value>) -> Option<Vec<String>> {
    let seeds = data?.get("seeds")?.as_array()?.iter().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>();
    (!seeds.is_empty()).then_some(seeds)
}

fn parse_wave_filters(settings: &Value, seeds: Vec<String>) -> Res<WaveFilters> {
    let selected_group = |title, options: Vec<(String, String)>, context| {
        let selected = options.iter().position(|(_, seed)| seeds.contains(seed));
        (!options.is_empty()).then_some(WaveSetting { title, options, selected, context })
    };
    let contexts = settings.get("blocks").and_then(Value::as_array).into_iter().flatten()
        .find(|block| block.get("type").and_then(Value::as_str) == Some("contexts"))
        .and_then(|block| block.get("items")).and_then(Value::as_array);
    let contexts = contexts.into_iter().flatten().filter_map(|context| {
        let title = s(context.get("name"))?;
        let kind = context.pointer("/id/type")?.as_str()?;
        let tag = context.pointer("/id/tag")?.as_str()?;
        Some((title, format!("{kind}:{tag}")))
    }).collect();
    let mut groups: Vec<_> = selected_group(crate::i18n::tr("По занятию"), contexts, true).into_iter().collect();
    groups.extend(["diversity", "moodEnergy", "language"].into_iter().filter_map(|key| {
        let group = settings.get("settingRestrictions")?.get(key)?;
        let title = s(group.get("name"))?;
        let options = group.get("possibleValues")?.as_array()?.iter().filter_map(|option| {
            if option.get("unspecified").and_then(Value::as_bool) == Some(true) {
                return None;
            }
            Some((s(option.get("name"))?, s(option.get("serializedSeed"))?))
        }).collect::<Vec<_>>();
        selected_group(title, options, false)
    }));
    if groups.is_empty() {
        return Err("Нет доступных настроек".into());
    }
    Ok(WaveFilters { seeds, groups })
}

#[cfg(test)]
#[test]
fn activity_replaces_station_and_keeps_selected_wave_filters() {
    let fixture = json!({
        "blocks": [{"type":"contexts","items":[{"id":{"type":"activity","tag":"road-trip"},"name":"В дороге"}]}],
        "settingRestrictions": {"moodEnergy":{"name":"Под настроение","possibleValues":[
            {"name":"Бодрое","serializedSeed":"settingMoodEnergy:active"},
            {"name":"Спокойное","serializedSeed":"settingMoodEnergy:calm"},
            {"name":"Любое","serializedSeed":"settingMoodEnergy:all","unspecified":true}
        ]}}
    });
    let mut settings = parse_wave_filters(&fixture, vec![
        WAVE.into(), "settingMoodEnergy:active".into(), "settingLanguage:russian".into(),
    ]).ok().unwrap();
    assert_eq!(settings.groups[1].selected, Some(0));
    settings.groups[0].selected = Some(0);
    settings.groups[1].selected = Some(1);
    assert_eq!(settings.context_title(), Some("В дороге"));
    assert_eq!(settings.selected_seeds(), ["activity:road-trip", "settingLanguage:russian", "settingMoodEnergy:calm"]);

    let mut activity = parse_wave_filters(&fixture, settings.selected_seeds()).ok().unwrap();
    activity.groups[0].selected = None;
    activity.groups[1].selected = None;
    assert_eq!(activity.selected_seeds(), [WAVE, "settingLanguage:russian"]);
}

fn parse_wheel_item(item: &Value) -> Option<WheelItem> {
    let data = item.get("data")?;
    let description = str_of(item, "description");
    let (name, cover, round, action) = match item.get("type").and_then(Value::as_str) {
        Some("ALBUM" | "NON_MUSIC") => {
            let album = parse_album(data.get("album")?)?;
            (album.title, album.cover, false, WheelAction::Album(album.id))
        }
        Some("PROMO_LINK") => {
            let url = data.pointer("/action/weblink")?.as_str()?;
            let url = if url.starts_with('/') && !url.starts_with("//") {
                format!("https://music.yandex.ru{url}")
            } else {
                url.to_owned()
            };
            let url = crate::client::browser_url(&url)?;
            let address = reqwest::Url::parse(&url).ok()?;
            let action = if address.host_str() == Some("music.yandex.ru")
                && matches!(address.path().trim_end_matches('/'), "/multivibe" | "/collection/multivibes")
            {
                WheelAction::SharedWave
            } else {
                WheelAction::Link(url)
            };
            (str_of(data, "title"), s(data.pointer("/cover/uri")), false, action)
        }
        Some("SETTING") => (str_of(data, "title"), s(data.pointer("/cover/uri")), false, WheelAction::Settings),
        Some("WAVE") | None => {
            let wave = data.get("wave")?;
            let seeds = wave.get("seeds")?.as_array()?.iter().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>();
            if seeds.is_empty() {
                return None;
            }
            let artist = data.pointer("/agent/entity/type").and_then(Value::as_str) == Some("artist");
            (str_of(wave, "name"), s(data.pointer("/agent/cover/uri")), artist, WheelAction::Play(seeds))
        }
        _ => return None,
    };
    (!name.is_empty()).then_some(WheelItem { name, description, cover, round, action })
}

fn room_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
}

pub fn wave_room_invitation(text: &str) -> Option<String> {
    let url = reqwest::Url::parse(text.trim()).ok()?;
    if url.scheme() != "https" || url.host_str() != Some("music.yandex.ru")
        || !url.username().is_empty() || url.password().is_some() || url.port().is_some()
    {
        return None;
    }
    let id = url.path().strip_prefix("/multivibe/")?.trim_end_matches('/');
    room_id(id).then(|| id.to_owned())
}

fn parse_wave_room(data: &Value) -> Option<WaveRoom> {
    let id = data.get("id")?.as_str()?.to_owned();
    if !room_id(&id) {
        return None;
    }
    let owner = data.get("owner");
    let owner_uid = owner.and_then(|owner| s(owner.get("uid")));
    let members = data.get("members").and_then(Value::as_array).into_iter().flatten()
        .filter(|member| s(member.get("uid")) != owner_uid || owner_uid.is_none());
    let names = owner.into_iter().chain(members).filter_map(|member| {
        member.get("name").and_then(Value::as_str).filter(|name| !name.trim().is_empty())
    }).collect::<Vec<_>>().join(" · ");
    let seeds = data.pointer("/wave/seeds").and_then(Value::as_array).into_iter().flatten()
        .filter_map(Value::as_str).filter(|seed| !seed.trim().is_empty()).map(str::to_owned).collect();
    Some(WaveRoom {
        id,
        name: str_of(data, "name"),
        members: names,
        status: str_of(data, "status"),
        seeds,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub struct WaveWords {
    pub text: String,
    pub action: Option<(String, String)>,
    pub fact: bool,
}

fn parse_wave_words(cards: &Value, track_id: &str) -> Option<WaveWords> {
    let group = cards.as_array()?.iter().find(|group| s(group.get("id")).as_deref() == Some(track_id))?;
    let card = group.get("cards")?.as_array()?.first()?.get("card")?;
    let tags = card.get("tags").and_then(Value::as_array);
    let tagged = |name| tags.is_some_and(|tags| tags.iter().any(|tag| tag.as_str() == Some(name)));
    let text = card.get("text").and_then(Value::as_str).unwrap_or_default().trim().to_owned();
    let action = card.get("action").and_then(|action| {
        let title = action.get("title")?.as_str()?.trim();
        let link = if tagged("DONATION") {
            action.get("link")
        } else {
            action.get("weblink").or_else(|| action.get("link"))
        }?.as_str()?.trim();
        if title.is_empty() || link.is_empty() {
            return None;
        }
        let target = crate::landing::web_target(link);
        if target.strip_prefix("url:").is_some_and(|url| crate::client::browser_url(url).is_none()) {
            return None;
        }
        Some((title.to_owned(), target))
    });
    (!text.is_empty() || action.is_some()).then_some(WaveWords { text, action, fact: tagged("FACT") })
}

#[cfg(test)]
#[test]
fn wheel_keeps_all_reshuffle_seeds_and_card_actions() {
    let wave = parse_wheel_item(&json!({
        "type": "WAVE",
        "data": {"wave": {"name": "Встряхнуть Мою волну", "seeds": [WAVE, "diversity:reshuffle"]}}
    })).unwrap();
    assert!(matches!(wave.action, WheelAction::Play(seeds) if seeds == [WAVE, "diversity:reshuffle"]));

    let settings = parse_wheel_item(&json!({"type": "SETTING", "data": {"title": "Настроить Мою волну"}})).unwrap();
    assert!(matches!(settings.action, WheelAction::Settings));

    let shared = parse_wheel_item(&json!({
        "type": "PROMO_LINK",
        "data": {"title": "Моя волна на двоих", "action": {"weblink": "/multivibe"}}
    })).unwrap();
    assert!(matches!(shared.action, WheelAction::SharedWave));
}

#[cfg(test)]
#[test]
fn shared_wave_requires_server_seeds_and_accepts_only_music_invites() {
    let draft = parse_wave_room(&json!({
        "id": "room-uid", "status": "DRAFT",
        "owner": {"uid": 12, "name":"Лена"}, "members": [{"uid":12, "name":"Лена"}]
    })).unwrap();
    assert!(!draft.playable());
    assert_eq!(draft.members, "Лена");
    let ready = parse_wave_room(&json!({
        "id": "room-uid", "name": "В пути", "status": "ENABLED",
        "owner": {"uid": 12, "name":"Лена"}, "members": [{"uid":13, "name":"Никита"}],
        "wave": {"seeds":["server-room:room-uid","server-mode:shared"]}
    })).unwrap();
    assert!(ready.playable());
    assert_eq!(ready.seeds, ["server-room:room-uid", "server-mode:shared"]);
    assert_eq!(wave_room_invitation(&ready.invitation()).as_deref(), Some("room-uid"));
    assert!(wave_room_invitation("https://music.yandex.ru@other.example/multivibe/1").is_none());
    assert!(wave_room_invitation("https://music.yandex.ru/multivibe/%2Fa").is_none());
    assert!(wave_room_invitation("https://music.yandex.ru/multivibe/").is_none());
}

#[cfg(test)]
#[test]
fn wave_words_use_the_first_card_for_the_requested_track() {
    let cards = json!([
        {"id": "7", "cards": [
            {"card": {"text": "На Станции ваши треки звучат без лишних нажатий", "action": {
                "title": "Выбрать Колонку", "weblink": "/speakers", "link": "https://example.org/unused"
            }}},
            {"card": {"tags": ["FACT"], "text": "Следующая карточка"}}
        ]},
        {"id": 8, "cards": [{"card": {"tags": ["FACT"], "text": "  Факт об артисте  "}}]}
    ]);
    let fact = parse_wave_words(&cards, "8").unwrap();
    assert!(fact.fact);
    assert_eq!(fact.text, "Факт об артисте");
    let promo = parse_wave_words(&cards, "7").unwrap();
    assert!(!promo.fact);
    assert_eq!(promo.action, Some(("Выбрать Колонку".into(), "url:https://music.yandex.ru/speakers".into())));
    assert!(parse_wave_words(&cards, "9").is_none());

    let invalid = json!([{"id": "7", "cards": [{"card": {"action": {"title": "Открыть", "link": "file:///test"}}}]}]);
    assert!(parse_wave_words(&invalid, "7").is_none());
}

#[cfg(test)]
#[test]
fn wave_cutout_prefers_release_then_artist_and_skips_regular_avatars() {
    let mut track = json!({
        "cutoutCover": {"uri": "avatars.yandex.net/get-music-content/release/%%"},
        "artists": [{"cutoutCover": {"uri": "//avatars.yandex.net/get-music-content/artist/%%"},
                     "cover": {"uri": "avatars.yandex.net/get-music-content/avatar/%%"}}]
    });
    assert_eq!(wave_cutout_url(&track).as_deref(), Some("https://avatars.yandex.net/get-music-content/release/orig"));
    track["cutoutCover"]["uri"] = json!("https://avatars.yandex.net@other.example/portrait/%%");
    assert_eq!(wave_cutout_url(&track).as_deref(), Some("https://avatars.yandex.net/get-music-content/artist/orig"));
    track["artists"][0]["cutoutCover"] = Value::Null;
    assert_eq!(wave_cutout_url(&track), None);
}

/// Язык ответов сервера (названия блоков, описания).
pub fn set_lang(l: &str) {
    *LANG.write().unwrap() = l.to_string();
}

fn lang() -> String {
    let l = LANG.read().unwrap();
    if l.is_empty() { "ru".into() } else { l.clone() }
}

/// Дата релиза по Москве: «13 октября 2026 г.».
fn release_date(iso: &str) -> String {
    const MONTHS: [&str; 12] = [
        "января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября", "ноября", "декабря",
    ];
    let n = |r: std::ops::Range<usize>| iso.get(r).and_then(|s| s.parse::<u32>().ok());
    let (Some(mut y), Some(mut m), Some(mut d)) = (n(0..4), n(5..7), n(8..10)) else { return String::new() };
    if !(1..=12).contains(&m) {
        return String::new();
    }
    if n(11..13).unwrap_or(0) + 3 >= 24 {
        let days = match m {
            2 if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
        d += 1;
        if d > days {
            d = 1;
            m += 1;
            if m > 12 {
                m = 1;
                y += 1;
            }
        }
    }
    format!("{d} {} {y} г.", MONTHS[m as usize - 1])
}

/// Карточка альбома в сетке: под названием исполнители и «2026 · Сингл».
pub fn album_card(a: &Album) -> Card {
    let year = a.year.map(|y| y.to_string()).unwrap_or_default();
    let kind = album_kind_word(&a.kind);
    Card {
        title: a.title.clone(),
        subtitle: a.artists.clone(),
        year: match (year.is_empty(), kind) {
            (_, "Альбом") => year,
            (true, k) => k.to_string(),
            (false, k) => format!("{year} · {k}"),
        },
        likes: None,
        covers: a.cover.iter().cloned().collect(),
        round: false,
        explicit: false,
        target: format!("album:{}", a.id),
    }
}

pub fn playlist_card(p: &Playlist) -> Card {
    Card {
        title: p.title.clone(),
        subtitle: String::new(),
        year: String::new(),
        likes: None,
        covers: p.covers.clone(),
        round: false,
        explicit: false,
        target: format!("playlist:{}:{}", p.uid, p.kind),
    }
}

pub fn artist_card(a: &Artist) -> Card {
    Card {
        title: a.name.clone(),
        subtitle: String::new(),
        year: String::new(),
        likes: None,
        covers: a.cover.iter().cloned().collect(),
        round: true,
        explicit: false,
        target: format!("artist:{}", a.id),
    }
}

/// Блок «Новый релиз» исполнителя: `{ release: { album, artists, releaseDate } }`.
fn release_card(v: &Value) -> Option<Card> {
    let rel = v.get("release")?;
    let al = merged(rel, "album")?;
    let a = parse_album(&al)?;
    let meta: Vec<String> = [release_date(&str_of(rel, "releaseDate")), album_kind_word(&a.kind).to_string()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();
    Some(Card {
        title: a.title.clone(),
        subtitle: a.artists.clone(),
        year: meta.join(" · "),
        likes: None,
        covers: a.cover.iter().cloned().collect(),
        round: false,
        explicit: false,
        target: format!("album:{}", a.id),
    })
}

pub fn album_kind_word(kind: &str) -> &'static str {
    match kind {
        "single" => "Сингл",
        "compilation" => "Сборник",
        "podcast" => "Подкаст",
        "audiobook" => "Аудиокнига",
        _ => "Альбом",
    }
}

fn parse_pin(v: &Value) -> Option<Pin> {
    let d = v.get("data")?;
    let cover = |p: &str| d.pointer(p).and_then(|u| u.as_str()).map(String::from);
    Some(match v.get("type")?.as_str()? {
        "album_item" => Pin {
            title: str_of(d, "title"),
            subtitle: album_kind_word(&str_of(d, "albumType")).into(),
            cover: cover("/cover/uri"),
            round: false,
            target: format!("album:{}", s(d.get("id"))?),
        },
        "playlist_item" => Pin {
            title: str_of(d, "title"),
            subtitle: "Плейлист".into(),
            cover: cover("/cover/uri"),
            round: false,
            target: format!("playlist:{}:{}", s(d.get("uid"))?, s(d.get("kind"))?),
        },
        "artist_item" => Pin {
            title: str_of(d, "name"),
            subtitle: "Артист".into(),
            cover: cover("/cover/uri"),
            round: true,
            target: format!("artist:{}", s(d.get("id"))?),
        },
        "wave_item" => Pin {
            title: str_of(d, "title"),
            subtitle: str_of(d, "header"),
            cover: cover("/backgroundImageUrl").or_else(|| cover("/agent/cover/uri")),
            round: true,
            target: format!("station:{}", str_of(d, "stationId")),
        },
        other => {
            crate::config::log(format!("закреп неизвестного типа {other}"));
            return None;
        }
    })
}

/// В блоках лендинга исполнители и дата релиза лежат рядом с сущностью, а не внутри неё.
pub(crate) fn merged(data: &Value, key: &str) -> Option<Value> {
    let mut v = data.get(key)?.clone();
    if let Some(obj) = v.as_object_mut() {
        for k in ["artists", "releaseDate", "likesCount", "trackCount"] {
            if !obj.contains_key(k) {
                if let Some(x) = data.get(k) {
                    obj.insert(k.to_string(), x.clone());
                }
            }
        }
    }
    Some(v)
}

/// Карточка из блока лендинга: альбом, плейлист, исполнитель, подборка.
#[derive(Clone, Debug)]
pub struct Card {
    pub title: String,
    pub subtitle: String,
    pub year: String,
    pub likes: Option<u64>,
    pub covers: Vec<String>,
    pub round: bool,
    pub explicit: bool,
    /// album:ID, playlist:UID:KIND, artist:ID, tag:ID
    pub target: String,
}

pub fn parse_card(it: &Value) -> Option<Card> {
    let d = it.get("data")?;
    let explicit = |v: &Value| v.get("contentWarning").and_then(|c| c.as_str()) == Some("explicit");
    Some(match it.get("type")?.as_str()? {
        "album_item" => {
            let v = merged(d, "album")?;
            let a = parse_album(&v)?;
            Card {
                title: a.title.clone(),
                subtitle: a.artists.clone(),
                year: a.year.map(|y| y.to_string()).unwrap_or_default(),
                likes: None,
                covers: a.cover.iter().cloned().collect(),
                round: false,
                explicit: explicit(&v),
                target: format!("album:{}", a.id),
            }
        }
        "liked_playlist_item" | "playlist_item" => {
            let v = merged(d, "playlist")?;
            let p = parse_playlist(&v)?;
            Card {
                title: p.title.clone(),
                subtitle: String::new(),
                year: String::new(),
                likes: d.get("likesCount").and_then(|x| x.as_u64()),
                covers: p.covers.clone(),
                round: false,
                explicit: false,
                target: format!("playlist:{}:{}", p.uid, p.kind),
            }
        }
        "artist_item" => {
            let a = parse_artist(d.get("artist")?)?;
            Card {
                title: a.name.clone(),
                subtitle: String::new(),
                year: String::new(),
                likes: None,
                covers: a.cover.iter().cloned().collect(),
                round: true,
                explicit: false,
                target: format!("artist:{}", a.id),
            }
        }
        "track_item" => {
            let t = parse_track(d.get("track")?)?;
            Card {
                title: t.title.clone(),
                subtitle: t.artists.clone(),
                year: String::new(),
                likes: None,
                covers: t.cover.iter().cloned().collect(),
                round: false,
                explicit: t.explicit,
                target: match &t.album_id {
                    Some(a) => format!("album:{a}"),
                    None => format!("track:{}", t.id),
                },
            }
        }
        "mix_card_item" => Card {
            title: str_of(d, "title"),
            subtitle: String::new(),
            year: String::new(),
            likes: None,
            covers: d
                .get("covers")
                .and_then(|c| c.as_array())
                .map(|a| a.iter().filter_map(|c| c.get("uri")?.as_str().map(String::from)).collect())
                .unwrap_or_default(),
            round: false,
            explicit: false,
            target: format!("tag:{}", d.pointer("/action/weblink").and_then(|w| w.as_str()).unwrap_or("").trim_start_matches("/tag/")),
        },
        _ => return None,
    })
}

fn cards(v: &Value) -> Vec<Card> {
    v.get("items").and_then(|i| i.as_array()).map(|a| a.iter().filter_map(parse_card).collect()).unwrap_or_default()
}

/// Строка чарта: трек, место и движение.
#[derive(Clone, Debug)]
pub struct ChartTrack {
    pub track: Track,
    pub position: u32,
    /// same, up, down, new
    pub progress: String,
}

/// Страница поиска до ввода запроса — блоки скелета `Search`.
#[derive(Clone, Debug, Default)]
pub struct SearchLanding {
    pub mixes: Vec<Card>,
    pub missed: Vec<Card>,
    pub new_playlists: Vec<Card>,
    pub chart: Vec<ChartTrack>,
    pub history: Vec<Card>,
}

/// Страница исполнителя — блоки скелета `artist_web`.
#[derive(Clone, Debug, Default)]
pub struct ArtistFull {
    pub id: String,
    pub name: String,
    pub cover: Option<String>,
    pub color: Option<[u8; 3]>,
    pub listeners: Option<u64>,
    pub popular: Vec<Track>,
    pub familiar_tracks: u64,
    pub familiar_albums: u64,
    pub studio: Vec<Card>,
    pub albums: Vec<Card>,
    pub playlists: Vec<Card>,
    pub compilations: Vec<Card>,
    pub similar: Vec<Card>,
    pub release: Option<Card>,
}

fn tabs<T>(v: &Value, key: &str, f: fn(&Value) -> Option<T>) -> Vec<(String, Vec<T>)> {
    v.get("tabs")
        .and_then(|t| t.as_array())
        .map(|tabs| {
            tabs.iter()
                .map(|t| {
                    let items = t
                        .get("items")
                        .and_then(|i| i.as_array())
                        .map(|a| a.iter().filter_map(|i| f(&merged(i.get("data")?, key)?)).collect())
                        .unwrap_or_default();
                    (str_of(t, "title"), items)
                })
                .filter(|(_, items): &(String, Vec<T>)| !items.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn parse_top_artist(v: &Value) -> Option<TopArtist> {
    Some(TopArtist {
        artist: parse_artist(v.get("artist")?)?,
        seconds: v.get("listenTimeSeconds").and_then(|x| x.as_u64()).unwrap_or(0),
        position: v.pointer("/top/position").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
        progress: v.pointer("/top/progress").and_then(|x| x.as_str()).unwrap_or("same").to_string(),
    })
}

fn results<T>(v: &Value, key: &str, f: fn(&Value) -> Option<T>) -> Vec<T> {
    v.pointer(&format!("/{key}/results"))
        .and_then(|r| r.as_array())
        .map(|a| a.iter().filter_map(f).collect())
        .unwrap_or_default()
}

/// LRC как у оригинала: берётся первая метка строки, конец строки — метка следующей
/// (в том числе пустой `[мм:сс.сс]` без текста), у последней конца может не быть.
fn parse_lrc(text: &str) -> Vec<SyncLine> {
    fn stamp(line: &str) -> Option<(f32, &str)> {
        let rest = line.trim_start().strip_prefix('[')?;
        let end = rest.find(']')?;
        let (m, sec) = rest[..end].split_once(':')?;
        let m: f32 = if m.is_empty() { 0.0 } else { m.parse().ok()? };
        let sec: f32 = if sec.is_empty() { 0.0 } else { sec.parse().ok()? };
        Some((m * 60.0 + sec, &rest[end + 1..]))
    }
    let tagged: Vec<(f32, &str)> = text.lines().filter_map(stamp).collect();
    let mut out = Vec::new();
    for (i, (from, rest)) in tagged.iter().enumerate() {
        if rest.is_empty() {
            continue;
        }
        out.push(SyncLine { from: *from, to: tagged.get(i + 1).map(|n| n.0), text: rest.trim().to_string() });
    }
    out
}

fn now() -> Duration {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default()
}

fn sign(msg: &str) -> Res<String> {
    let mut mac = Hmac::<Sha256>::new_from_slice(SIGN_KEY.as_bytes()).map_err(|e| e.to_string())?;
    mac.update(msg.as_bytes());
    Ok(base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes()))
}

fn xml_tag<'a>(xml: &'a str, name: &str) -> Res<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = xml.find(&open).ok_or(format!("нет <{name}> в download-info"))? + open.len();
    let end = xml[start..].find(&close).ok_or(format!("нет </{name}>"))? + start;
    Ok(&xml[start..end])
}

fn short(s: &str) -> String {
    s.chars().take(200).collect()
}

impl Api {
    pub fn new(token: &str) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(10))
            .user_agent("Yandex-Music-API")
            .gzip(true)
            .build()
            .expect("http client");
        Api { http, token: token.trim().to_string() }
    }

    /// Настройки аккаунта: `aiContentReductionEnabled` и прочее.
    pub fn account_settings(&self) -> Res<Value> {
        self.get("/account/settings", &[])
    }

    pub fn set_account_setting(&self, key: &str, value: &str) -> Res<()> {
        self.send(self.req(Method::POST, "/account/settings").form(&[(key, value)])).map(|_| ())
    }

    /// Запрос к API или по адресу из ответа сервера. Токен добавляется, только если адрес
    /// https и на хосте Яндекса: подменённый ответ не уведёт его на чужой сервер.
    fn req(&self, method: Method, path_or_url: &str) -> RequestBuilder {
        self.req_as(method, path_or_url, ANDROID_CLIENT)
    }

    /// Запрос от имени десктопного клиента, с теми же заголовками, что у оригинала.
    /// Заголовок клиента ровно один: reqwest не заменяет заголовок, а добавляет второй.
    fn desktop_req(&self, method: Method, path: &str) -> RequestBuilder {
        self.req_as(method, path, crate::client::desktop_client())
            .header("X-Yandex-Music-Device", crate::client::device_header())
            .header("X-Yandex-Music-Without-Invocation-Info", "1")
    }

    fn req_as(&self, method: Method, path_or_url: &str, client: &str) -> RequestBuilder {
        let url = if path_or_url.starts_with("https://") || path_or_url.starts_with("http://") {
            path_or_url.to_string()
        } else {
            format!("{BASE}{path_or_url}")
        };
        let trusted = reqwest::Url::parse(&url).is_ok_and(|u| token_allowed(&u));
        let mut rb = self
            .http
            .request(method, url)
            .header("X-Yandex-Music-Client", client)
            .header("Accept-Language", lang());
        if trusted && !self.token.is_empty() {
            rb = rb.header("Authorization", format!("OAuth {}", self.token));
        }
        rb
    }

    fn send_text(&self, rb: RequestBuilder) -> Res<(reqwest::StatusCode, String)> {
        let resp = rb.send().map_err(|e| format!("сеть: {}", crate::config::chain(&e)))?;
        let status = resp.status();
        let text = resp.text().map_err(|e| format!("сеть: {}", crate::config::chain(&e)))?;
        Ok((status, text))
    }

    /// Запрос к API, возвращает поле `result`.
    fn send(&self, rb: RequestBuilder) -> Res<Value> {
        let (status, text) = self.send_text(rb)?;
        Self::parse_result(status, &text)
    }

    fn parse_result(status: reqwest::StatusCode, text: &str) -> Res<Value> {
        let v: Value = serde_json::from_str(text).map_err(|_| format!("HTTP {status}: {}", short(text)))?;
        if !status.is_success() {
            let msg = v
                .pointer("/error/message")
                .or_else(|| v.pointer("/error/name"))
                .and_then(|m| m.as_str())
                .unwrap_or("");
            if status.as_u16() == 401 || status.as_u16() == 403 {
                return Err(format!("нет доступа (HTTP {status}) — токен не подошёл или истёк. {msg}"));
            }
            return Err(format!("HTTP {status}: {msg}"));
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }

    fn get(&self, path: &str, query: &[(&str, &str)]) -> Res<Value> {
        self.send(self.req(Method::GET, path).query(query))
    }

    pub fn account(&self) -> Result<Account, AccountError> {
        let (status, text) = self.send_text(self.req(Method::GET, "/account/status"))
            .map_err(AccountError::Unavailable)?;
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(AccountError::Unauthorized);
        }
        let v = Self::parse_result(status, &text).map_err(AccountError::Unavailable)?;
        let acc = v.get("account").ok_or_else(|| AccountError::Unavailable("в ответе нет account".into()))?;
        let uid = s(acc.get("uid")).ok_or(AccountError::Unauthorized)?;
        Ok(Account {
            uid,
            login: str_of(acc, "login"),
            has_plus: v.pointer("/plus/hasPlus").and_then(|x| x.as_bool()).unwrap_or(false),
        })
    }

    /// Имя для показа и ссылка на аватар 100×100.
    pub fn about(&self) -> Res<(String, Option<String>)> {
        let v = self.get("/account/about", &[])?;
        let name = s(v.get("publicName")).filter(|n| !n.is_empty()).unwrap_or_else(|| str_of(&v, "login"));
        let avatar = s(v.get("avatarId")).map(|id| format!("https://avatars.yandex.net/get-yapic/{id}/islands-retina-50"));
        Ok((name, avatar))
    }

    /// Блоки лендингов отвечают так же, как официальному приложению для Windows.
    pub fn get_raw(&self, path: &str) -> Res<Value> {
        self.send_raw(self.desktop_req(Method::GET, path))
    }

    pub fn post_raw(&self, path: &str, body: &Value) -> Res<Value> {
        self.send_raw(self.desktop_req(Method::POST, path).json(body))
    }

    fn send_raw(&self, rb: RequestBuilder) -> Res<Value> {
        let (status, text) = self.send_text(rb)?;
        if !status.is_success() {
            return Err(format!("HTTP {status}: {}", short(&text)));
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        Ok(v.get("result").cloned().unwrap_or(v))
    }

    /// Трейлер исполнителя, альбома или плейлиста: заголовок и несколько треков.
    /// `path` — `artists/<id>`, `albums/<id>` или `users/<uid>/playlists/<kind>`.
    pub fn trailer(&self, path: &str) -> Res<(String, Vec<Track>)> {
        let v = self.get_raw(&format!("/{path}/trailer"))?;
        let t = v.get("trailer").unwrap_or(&v);
        let title = t.get("title").and_then(|x| x.as_str()).unwrap_or("Трейлер").to_string();
        let tracks: Vec<Track> = t
            .get("tracks")
            .and_then(|a| a.as_array())
            .map(|a| a.iter().filter_map(parse_track).collect())
            .or_else(|| v.get("track").and_then(parse_track).map(|t| vec![t]))
            .unwrap_or_default();
        if tracks.is_empty() {
            return Err("трейлер недоступен".into());
        }
        Ok((title, tracks))
    }

    /// Обложка «Мне нравится» — это плейлист kind 3.
    pub fn likes_cover(&self, uid: &str) -> Option<String> {
        let v = self.get(&format!("/users/{uid}/playlists/3"), &[]).ok()?;
        v.pointer("/cover/uri").and_then(|u| u.as_str()).map(String::from)
    }

    pub fn collection(&self, uid: &str) -> Res<Collection> {
        let likes = self.get_raw("/landing-blocks/collection/playlist-with-likes?count=8")?;
        let top = self.get_raw("/personal/top/artists/month?count=5").unwrap_or_default();
        let pls = self.get_raw("/landing-blocks/collection/playlists-liked-and-playlists-created?count=8").unwrap_or_default();
        let albums = self.get_raw("/landing-blocks/collection/albums-liked-and-albums-presaved?count=8").unwrap_or_default();
        let likes_cover = self.likes_cover(uid);
        Ok(Collection {
            likes_count: likes.pointer("/summary/count").and_then(|c| c.as_u64()).unwrap_or(0),
            likes_cover,
            likes_preview: likes.get("tracks").and_then(|t| t.as_array()).map(|a| a.iter().filter_map(parse_track).collect()).unwrap_or_default(),
            top_artists: top.get("artists").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(parse_top_artist).collect()).unwrap_or_default(),
            playlist_tabs: tabs(&pls, "playlist", parse_playlist),
            album_tabs: tabs(&albums, "album", parse_album),
        })
    }

    pub fn search_suggestions(&self) -> Res<Vec<String>> {
        let data = self.get_raw("/search/suggestions/q2v")?;
        Ok(data.get("items").and_then(Value::as_array).into_iter().flatten()
            .filter_map(|item| item.get("query").and_then(Value::as_str).map(str::to_owned)).collect())
    }

    pub fn search_landing(&self) -> Res<SearchLanding> {
        let chart = self.get_raw("/landing/block/chart").unwrap_or_default();
        Ok(SearchLanding {
            mixes: cards(&self.get_raw("/landing/block/mixes/mixes_music")?),
            missed: cards(&self.get_raw("/landing/block/editorial/compilation/foreign").unwrap_or_default()),
            new_playlists: cards(&self.get_raw("/landing-blocks/new-playlists").unwrap_or_default()),
            chart: chart
                .pointer("/chart/tracks")
                .and_then(|t| t.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| {
                            Some(ChartTrack {
                                track: parse_track(x.get("track")?)?,
                                position: x.pointer("/chart/position").and_then(|p| p.as_u64()).unwrap_or(0) as u32,
                                progress: x.pointer("/chart/progress").and_then(|p| p.as_str()).unwrap_or("same").to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
            history: cards(&self.get_raw("/landing-blocks/history").unwrap_or_default()),
        })
    }

    pub fn artist_full(&self, id: &str) -> Res<ArtistFull> {
        let info = self.get_raw(&format!("/artists/{id}/info"))?;
        let a = info.get("artist").and_then(parse_artist).ok_or("нет данных об исполнителе")?;
        let tracks = self.get_raw(&format!("/artists/{id}/tracks")).unwrap_or_default();
        let fam = self.get_raw(&format!("/artists/{id}/familiar-you/info?withWaveInfo=true&withCollectionInfo=true")).unwrap_or_default();
        let block = |b: &str| cards(&self.get_raw(&format!("/artists/{id}/blocks/{b}")).unwrap_or_default());
        Ok(ArtistFull {
            id: a.id.clone(),
            name: a.name.clone(),
            cover: a.cover.clone(),
            color: a.color,
            listeners: info.pointer("/stats/lastMonthListeners").and_then(|x| x.as_u64()),
            popular: tracks.get("tracks").and_then(|t| t.as_array()).map(|a| a.iter().filter_map(parse_track).take(5).collect()).unwrap_or_default(),
            familiar_tracks: fam.pointer("/collection/trackCount").and_then(|x| x.as_u64()).unwrap_or(0)
                + fam.pointer("/wave/trackCount").and_then(|x| x.as_u64()).unwrap_or(0),
            familiar_albums: fam.pointer("/collection/albumCount").and_then(|x| x.as_u64()).unwrap_or(0),
            studio: block("artist-studio-albums"),
            albums: block("artist-albums"),
            playlists: block("artist-playlists"),
            compilations: block("artist-compilations"),
            similar: block("similar-artists"),
            release: self.get_raw(&format!("/artists/{id}/blocks/artist-release")).ok().and_then(|v| release_card(&v)),
        })
    }

    /// Закрепить в боковой панели или открепить: album:ID, artist:ID, playlist:UID:KIND, station:ID.
    pub fn pin(&self, target: &str, pin: bool) -> Res<()> {
        let method = if pin { Method::PUT } else { Method::DELETE };
        let (path, query, body): (&str, Vec<(&str, String)>, Value) = if let Some(st) = target.strip_prefix("station:") {
            ("/pin/wave", vec![("seeds", st.to_string())], json!({ "seeds": [st] }))
        } else {
            match target.split(':').collect::<Vec<_>>().as_slice() {
                ["album", id] => ("/pin/album", vec![("id", id.to_string())], json!({ "id": id })),
                ["artist", id] => ("/pin/artist", vec![("id", id.to_string())], json!({ "id": id })),
                ["playlist", uid, kind] => {
                    ("/pin/playlist", vec![("uid", uid.to_string()), ("kind", kind.to_string())], json!({ "uid": uid, "kind": kind }))
                }
                _ => return Err(format!("нельзя закрепить {target}")),
            }
        };
        let (status, text) = self.send_text(self.desktop_req(method, path).query(&query).json(&body))?;
        if status.is_success() {
            Ok(())
        } else {
            Err(format!("HTTP {status}: {}", short(&text)))
        }
    }

    /// Лайк альбома или исполнителя: `kind` — albums или artists.
    pub fn like_entity(&self, uid: &str, kind: &str, id: &str, remove: bool) -> Res<()> {
        let action = if remove { "remove" } else { "add-multiple" };
        let field = match kind {
            "albums" => "album-ids",
            "playlists" => "playlist-ids",
            _ => "artist-ids",
        };
        self.send(self.req(Method::POST, &format!("/users/{uid}/likes/{kind}/{action}")).form(&[(field, id)]))
            .map(|_| ())
    }

    /// Вкладки «Смотреть всё» коллекции: block — playlists-liked-and-playlists-created или albums-liked-and-albums-presaved.
    pub fn collection_tabs(&self, block: &str) -> Res<Vec<(String, Vec<Card>)>> {
        let v = self.get_raw(&format!("/landing-blocks/collection/{block}?count=500"))?;
        Ok(if block.starts_with("albums") {
            tabs(&v, "album", parse_album).into_iter().map(|(t, a)| (t, a.iter().map(album_card).collect())).collect()
        } else {
            tabs(&v, "playlist", parse_playlist).into_iter().map(|(t, p)| (t, p.iter().map(playlist_card).collect())).collect()
        })
    }

    /// Все альбомы исполнителя: path — direct-albums или also-albums, sort — year или rating.
    pub fn artist_albums(&self, id: &str, path: &str, sort: &str) -> Res<Vec<Card>> {
        let v = self.get_raw(&format!("/artists/{id}/{path}?page=0&page-size=200&sort-by={sort}"))?;
        Ok(v.get("albums").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(parse_album).map(|a| album_card(&a)).collect()).unwrap_or_default())
    }

    pub fn similar_artists(&self, id: &str) -> Res<Vec<Card>> {
        let v = self.get_raw(&format!("/artists/{id}/similar"))?;
        Ok(v.get("similarArtists")
            .and_then(|a| a.as_array())
            .map(|a| a.iter().filter_map(parse_artist).map(|a| artist_card(&a)).collect())
            .unwrap_or_default())
    }

    /// Понравившиеся исполнители, как на странице «Исполнители» коллекции.
    pub fn liked_artists(&self, uid: &str) -> Res<Vec<Artist>> {
        let mut out = Vec::new();
        for page in 0..10 {
            let v = self.get_raw(&format!("/users/{uid}/likes/artists/page?page={page}&pageSize=100"))?;
            let batch: Vec<Artist> = v.get("artists").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(parse_artist).collect()).unwrap_or_default();
            let n = batch.len();
            out.extend(batch);
            if n < 100 {
                break;
            }
        }
        Ok(out)
    }

    /// Понравившиеся плейлисты как цели: playlist:UID:KIND.
    pub fn liked_playlist_keys(&self, uid: &str) -> Res<Vec<String>> {
        let v = self.get(&format!("/users/{uid}/likes/playlists"), &[])?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| {
                        let p = x.get("playlist")?;
                        Some(format!("playlist:{}:{}", s(p.get("uid")).or_else(|| s(p.pointer("/owner/uid")))?, s(p.get("kind"))?))
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub fn liked_album_ids(&self, uid: &str) -> Res<Vec<String>> {
        let v = self.get(&format!("/users/{uid}/likes/albums"), &[])?;
        Ok(v.as_array().map(|a| a.iter().filter_map(|x| s(x.get("id"))).collect()).unwrap_or_default())
    }

    pub fn wheel(&self) -> Res<Vec<WheelItem>> {
        let v = self.post_raw("/wheel/new", &json!({}))?;
        Ok(v.get("items").and_then(Value::as_array).into_iter().flatten().filter_map(parse_wheel_item).collect())
    }

    pub fn wave_cutout(&self, track: &Track) -> Res<Option<String>> {
        let valid_id = |id: &str| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit());
        if !valid_id(&track.id) || !track.album_id.as_deref().is_none_or(valid_id) {
            return Ok(None);
        }
        let result = self.get_raw(&format!("/tracks/{}", track.full_id()))?;
        Ok(result.as_array().into_iter().flatten()
            .find(|item| s(item.get("id")).as_deref() == Some(track.id.as_str()))
            .and_then(wave_cutout_url))
    }

    pub fn wave_words(&self, track_id: &str, seeds: &[String]) -> Res<Option<WaveWords>> {
        let cards = self.post_raw("/words/cards", &json!({
            "trackIds": [track_id],
            "viewedCards": [],
            "viewedBigCards": [],
            "actionProvider": "lumen",
            "waveContext": { "seeds": seeds },
        }))?;
        Ok(parse_wave_words(&cards, track_id))
    }

    pub fn wave_filters(&self, current: Option<&[String]>) -> Res<WaveFilters> {
        let seeds = current.map(<[String]>::to_vec).or_else(|| self.wave_settings()).unwrap_or_else(|| vec![WAVE.to_owned()]);
        let mut request = self.desktop_req(Method::GET, "/rotor/wave/settings");
        for seed in &seeds {
            request = request.query(&[("seeds", seed)]);
        }
        parse_wave_filters(&self.send_raw(request)?, seeds)
    }

    pub fn pins(&self) -> Res<Vec<Pin>> {
        let v = self.get("/pins", &[])?;
        Ok(v.get("pins").and_then(|p| p.as_array()).map(|a| a.iter().filter_map(parse_pin).collect()).unwrap_or_default())
    }

    pub fn tracks(&self, ids: &[String]) -> Res<Vec<Track>> {
        let mut out = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(250) {
            let v = self.send(self.req(Method::POST, "/tracks").form(&[("track-ids", chunk.join(","))]))?;
            if let Some(a) = v.as_array() {
                out.extend(a.iter().filter_map(parse_track));
            }
        }
        Ok(out)
    }

    // ---------- волна ----------

    fn session_req(&self, path: &str, body: &Value) -> Res<Value> {
        let mut rb = self.desktop_req(Method::POST, path).json(body);
        if LESS_AI.load(std::sync::atomic::Ordering::Relaxed) {
            rb = rb.header("X-Yandex-Music-AI-Content-Rate", "reduced");
        }
        self.send_raw(rb)
    }

    /// Треки волны через сессию ротора, как у оригинала.
    ///
    /// `Session::Next` — следующий пакет: в `queue` уходят трек, на котором сменился трек, и
    /// тот, что заиграл после него, а с ними отложенные фидбеки (пропуск, дослушал), чтобы
    /// сервер подбирал треки уже с их учётом. `Session::Clone` — продолжение волны после
    /// перезапуска: сервер заводит новую сессию с историей старой. Если сессия истекла или
    /// её нет, начинается новая.
    pub fn wave_session(
        &self, station: &str, selected_seeds: Option<Vec<String>>, session: WaveSession,
        queue: &[String], feedbacks: &[Value], incognito: bool,
    ) -> Res<Batch> {
        match session {
            WaveSession::Next(id) => {
                let mut body = json!({ "queue": queue });
                if !feedbacks.is_empty() {
                    body["feedbacks"] = json!(feedbacks);
                }
                match self.session_req(&format!("/rotor/session/{id}/tracks"), &body) {
                    Ok(v) => return parse_batch(&v, id.to_string()),
                    Err(e) => crate::config::log(format!("сессия {id}: {e}, начинаем новую")),
                }
            }
            WaveSession::Clone(id) => {
                let mut body = json!({
                    "queue": queue,
                    "includeTracksInResponse": true,
                    "includeWaveModel": true,
                    "interactive": false,
                });
                if !allow_explicit() {
                    body["allowExplicit"] = json!(false);
                }
                if incognito {
                    body["incognito"] = json!(true);
                }
                match self.session_req(&format!("/rotor/session/{id}/clone"), &body) {
                    Ok(v) => {
                        let id = str_of(&v, "radioSessionId");
                        return parse_batch(&v, id);
                    }
                    Err(e) => crate::config::log(format!("сессия {id}: {e}, начинаем новую")),
                }
            }
            WaveSession::New => {}
        }
        let seeds = selected_seeds.unwrap_or_else(|| {
            vec![station.to_owned()]
        });
        let mut body = json!({
            "seeds": seeds,
            "includeTracksInResponse": true,
            "includeWaveModel": true,
            "interactive": true,
        });
        if !allow_explicit() {
            body["allowExplicit"] = json!(false);
        }
        if incognito {
            body["incognito"] = json!(true);
        }
        if !queue.is_empty() {
            body["queue"] = json!(queue);
        }
        let v = self.session_req("/rotor/session/new", &body)?;
        let id = str_of(&v, "radioSessionId");
        let mut batch = parse_batch(&v, id)?;
        batch.seeds.get_or_insert(seeds);
        Ok(batch)
    }

    pub fn wave_rooms(&self) -> Res<Vec<WaveRoom>> {
        let rooms = self.get_raw("/wave/rooms")?;
        let rooms = rooms.as_array().ok_or("Некорректный список совместных волн")?;
        Ok(rooms.iter().filter_map(parse_wave_room).collect())
    }

    pub fn create_wave_room(&self) -> Res<WaveRoom> {
        let room = self.send_raw(self.desktop_req(Method::POST, "/wave/rooms"))?;
        parse_wave_room(&room).ok_or_else(|| "Некорректный ответ при создании волны".into())
    }

    pub fn join_wave_room(&self, id: &str) -> Res<WaveRoom> {
        let enter = |id: &str| self.desktop_req(Method::PUT, &format!("/wave/rooms/{id}/enter"));
        let (status, text) = self.send_text(enter(id))?;
        let response: Value = serde_json::from_str(&text).map_err(|_| format!("HTTP {status}"))?;
        if status.is_success() {
            return parse_wave_room(response.get("result").unwrap_or(&response))
                .ok_or_else(|| "Некорректный ответ при входе в волну".into());
        }
        let error = response.get("error").filter(|error| error.is_object()).unwrap_or(&response);
        if ["name", "errorName"].into_iter().any(|key| error.get(key).and_then(Value::as_str) == Some("ROOM_DUPLICATION")) {
            if let Some(existing) = error.pointer("/details/correctRoomId").and_then(Value::as_str).filter(|id| room_id(id)) {
                let room = self.send_raw(enter(existing))?;
                return parse_wave_room(&room).ok_or_else(|| "Некорректный ответ при входе в волну".into());
            }
        }
        Err(format!("HTTP {status}"))
    }

    pub fn rename_wave_room(&self, id: &str, name: &str) -> Res<()> {
        self.wave_room_change(self.desktop_req(Method::PUT, &format!("/wave/rooms/{id}")).json(&json!({ "name": name })))
    }

    pub fn leave_wave_room(&self, id: &str) -> Res<()> {
        self.wave_room_change(self.desktop_req(Method::PUT, &format!("/wave/rooms/{id}/exit")))
    }

    fn wave_room_change(&self, request: RequestBuilder) -> Res<()> {
        let status = request.send().map_err(|e| format!("сеть: {}", crate::config::chain(&e)))?.status();
        if status.is_success() { Ok(()) } else { Err(format!("HTTP {status}")) }
    }

    /// История прослушиваний: оригинал шлёт её при каждой смене трека. Без неё сервер не знает,
    /// что звучало, и рекомендации (волна в том числе) не подстраиваются под вкус.
    pub fn plays(&self, uid: &str, play: &Value) -> Res<()> {
        let now = iso_now();
        let rb = self
            .desktop_req(Method::POST, "/plays")
            .header("X-Yandex-Music-Multi-Auth-User-Id", uid)
            .query(&[("clientNow", now.as_str())])
            .json(&json!({ "plays": [play] }));
        let (status, text) = self.send_text(rb)?;
        if status.is_success() {
            Ok(())
        } else {
            Err(format!("HTTP {status}: {}", short(&text)))
        }
    }

    /// Настройки «Моей волны» («По характеру», «Под настроение», «По языку»), выбранные в любом
    /// клиенте: seeds `setting…` последней волны, если это была «Моя волна».
    pub fn wave_settings(&self) -> Option<Vec<String>> {
        let v = match self.get_raw("/rotor/wave/last") {
            Ok(v) => v,
            Err(e) => {
                crate::config::log(format!("rotor/wave/last: {e}"));
                return None;
            }
        };
        let seeds = wave_seeds(Some(&v))?;
        personal_wave_seeds(&seeds).then_some(seeds)
    }

    /// Отправить фидбек сессии (тело из [`session_event`]).
    pub fn session_feedback(&self, session: &str, body: &Value) -> Res<()> {
        self.session_req(&format!("/rotor/session/{session}/feedback/"), body).map(|_| ())
    }

    /// Фидбек станции: radioStarted / trackStarted / trackFinished / skip.
    pub fn feedback(&self, station: &str, kind: &str, track: Option<&str>, played: Option<f64>, batch_id: &str, from: &str) -> Res<()> {
        let mut body = json!({ "type": kind, "timestamp": now().as_secs_f64() });
        if kind == "radioStarted" {
            body["from"] = json!(from);
        }
        if let Some(t) = track {
            body["trackId"] = json!(t);
        }
        if let Some(p) = played {
            body["totalPlayedSeconds"] = json!(p);
        }
        let mut rb = self.req(Method::POST, &format!("/rotor/station/{station}/feedback")).json(&body);
        if !batch_id.is_empty() {
            rb = rb.query(&[("batch-id", batch_id)]);
        }
        self.send(rb).map(|_| ())
    }

    // ---------- лайки ----------

    pub fn liked_ids(&self, uid: &str) -> Res<Vec<String>> {
        let v = self.get(&format!("/users/{uid}/likes/tracks"), &[])?;
        Ok(v.pointer("/library/tracks")
            .and_then(|t| t.as_array())
            .map(|a| a.iter().filter_map(|x| s(x.get("id"))).collect())
            .unwrap_or_default())
    }

    pub fn liked_tracks(&self, uid: &str) -> Res<Vec<Track>> {
        let ids = self.liked_ids(uid)?;
        let mut tracks = self.tracks(&ids)?;
        let pos: std::collections::HashMap<&str, usize> = ids.iter().enumerate().map(|(i, id)| (id.as_str(), i)).collect();
        tracks.sort_by_key(|t| pos.get(t.id.as_str()).copied().unwrap_or(usize::MAX));
        Ok(tracks)
    }

    pub fn like(&self, uid: &str, track_full_id: &str, remove: bool) -> Res<()> {
        let action = if remove { "remove" } else { "add-multiple" };
        self.send(self.req(Method::POST, &format!("/users/{uid}/likes/tracks/{action}")).form(&[("track-ids", track_full_id)]))
            .map(|_| ())
    }

    pub fn dislike(&self, uid: &str, track_full_id: &str) -> Res<()> {
        self.send(
            self.req(Method::POST, &format!("/users/{uid}/dislikes/tracks/add-multiple"))
                .form(&[("track-ids", track_full_id)]),
        )
        .map(|v| crate::config::log(format!("dislike {track_full_id}: {v}")))
    }

    // ---------- каталог ----------

    pub fn search(&self, text: &str, kind: &str) -> Res<Search> {
        let v = self.get("/search", &[("text", text), ("type", kind), ("page", "0"), ("nocorrect", "false")])?;
        Ok(Search {
            tracks: results(&v, "tracks", parse_track),
            artists: results(&v, "artists", parse_artist),
            albums: results(&v, "albums", parse_album),
            playlists: results(&v, "playlists", parse_playlist),
        })
    }

    pub fn artist_tracks(&self, id: &str) -> Res<Vec<Track>> {
        let mut out = Vec::new();
        for page in 0..10 {
            let p = page.to_string();
            let v = self.get(&format!("/artists/{id}/tracks"), &[("page", &p), ("page-size", "100")])?;
            let got: Vec<Track> = v.get("tracks").and_then(|t| t.as_array()).map(|a| a.iter().filter_map(parse_track).collect()).unwrap_or_default();
            let total = v.pointer("/pager/total").and_then(|t| t.as_u64()).unwrap_or(0) as usize;
            let empty = got.is_empty();
            out.extend(got);
            if empty || out.len() >= total {
                break;
            }
        }
        Ok(out)
    }

    pub fn album(&self, id: &str) -> Res<(Album, Vec<Track>)> {
        let v = self.get(&format!("/albums/{id}/with-tracks"), &[])?;
        let album = parse_album(&v).ok_or("нет данных об альбоме")?;
        let tracks = v
            .get("volumes")
            .and_then(|x| x.as_array())
            .map(|vols| vols.iter().filter_map(|d| d.as_array()).flatten().filter_map(parse_track).collect())
            .unwrap_or_default();
        Ok((album, tracks))
    }

    pub fn playlist(&self, uid: &str, kind: &str) -> Res<(Playlist, Vec<Track>)> {
        let v = self.get(&format!("/users/{uid}/playlists/{kind}"), &[])?;
        let pl = parse_playlist(&v).ok_or("нет данных о плейлисте")?;
        let items = v.get("tracks").and_then(|t| t.as_array()).cloned().unwrap_or_default();
        let revision = v.get("revision").and_then(Value::as_u64);
        let mut tracks: Vec<Track> = items.iter().enumerate().filter_map(|(index, i)| Some(playlist_entry_track(i, parse_track(i.get("track")?)?, revision, index))).collect();
        if tracks.is_empty() && !items.is_empty() {
            let ids: Vec<String> = items.iter().filter_map(|i| s(i.get("id"))).collect();
            let found: std::collections::HashMap<_, _> = self.tracks(&ids)?.into_iter().map(|t| (t.id.clone(), t)).collect();
            tracks = items.iter().enumerate().filter_map(|(index, item)| {
                let track = found.get(&s(item.get("id"))?)?;
                Some(playlist_entry_track(item, track.clone(), revision, index))
            }).collect();
        }
        Ok((pl, tracks))
    }

    pub fn playlists(&self, uid: &str) -> Res<Vec<Playlist>> {
        let v = self.get(&format!("/users/{uid}/playlists/list"), &[])?;
        Ok(v.as_array().map(|a| a.iter().filter_map(parse_playlist).collect()).unwrap_or_default())
    }

    pub fn create_playlist(&self, uid: &str, title: &str) -> Res<Playlist> {
        let v = self.send(
            self.req(Method::POST, &format!("/users/{uid}/playlists/create"))
                .form(&[("title", title), ("visibility", "private")]),
        )?;
        parse_playlist(&v).ok_or_else(|| "Яндекс не вернул новый плейлист".into())
    }

    /// Добавляет треки в начало плейлиста одним запросом, порядок сохраняется.
    /// Ревизию берём свежую, иначе API отвечает 412.
    pub fn add_to_playlist(&self, uid: &str, kind: &str, tracks: &[Track]) -> Res<()> {
        let v = self.get(&format!("/users/{uid}/playlists/{kind}"), &[])?;
        let revision = v.get("revision").and_then(|r| r.as_u64()).unwrap_or(1).to_string();
        let items: Vec<_> = tracks
            .iter()
            .map(|t| {
                let mut item = json!({ "id": t.id });
                if let Some(a) = &t.album_id {
                    item["albumId"] = json!(a);
                }
                item
            })
            .collect();
        let diff = json!([{ "op": "insert", "at": 0, "tracks": items }]).to_string();
        self.send(
            self.req(Method::POST, &format!("/users/{uid}/playlists/{kind}/change-relative"))
                .form(&[("kind", kind), ("revision", revision.as_str()), ("diff", diff.as_str())]),
        )
        .map(|_| ())
    }

    /// Индекс и ревизия должны относиться к одному снимку плейлиста.
    pub fn remove_from_playlist(&self, uid: &str, kind: &str, t: &Track) -> Res<()> {
        let v = self.get(&format!("/users/{uid}/playlists/{kind}"), &[])?;
        let revision = v.get("revision").and_then(Value::as_u64).ok_or("нет ревизии плейлиста")?.to_string();
        let at = playlist_remove_index(&v, t)?;
        let mut item = json!({ "id": t.id });
        if let Some(a) = t.playlist_slot.as_ref().and_then(|slot| slot.album.as_ref()).or(t.album_id.as_ref()) {
            item["albumId"] = json!(a);
        }
        let diff = json!([{ "op": "delete", "from": at, "to": at + 1, "tracks": [item] }]).to_string();
        self.send(
            self.req(Method::POST, &format!("/users/{uid}/playlists/{kind}/change-relative"))
                .form(&[("kind", kind), ("revision", revision.as_str()), ("diff", diff.as_str())]),
        )
        .map(|_| ())
    }

    /// Название (`name`) или описание (`description`) плейлиста.
    pub fn set_playlist_field(&self, uid: &str, kind: &str, field: &str, value: &str) -> Res<()> {
        self.send(self.req(Method::POST, &format!("/users/{uid}/playlists/{kind}/{field}")).query(&[("value", value)])).map(|_| ())
    }

    pub fn delete_playlist(&self, uid: &str, kind: &str) -> Res<()> {
        self.send(self.req(Method::POST, &format!("/users/{uid}/playlists/{kind}/delete"))).map(|_| ())
    }

    /// Загружает обложку (поле формы `image`, как у оригинала), возвращает адрес новой.
    pub fn upload_playlist_cover(&self, uid: &str, kind: &str, file: &str, bytes: Vec<u8>) -> Res<String> {
        let boundary = format!("----yml{:x}", now().as_nanos());
        let mime = if file.to_lowercase().ends_with(".png") { "image/png" } else { "image/jpeg" };
        let name = file.rsplit(['\\', '/']).next().unwrap_or("cover.jpg").replace('"', "");
        let mut body = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"{name}\"\r\nContent-Type: {mime}\r\n\r\n").into_bytes();
        body.extend(bytes);
        body.extend(format!("\r\n--{boundary}--\r\n").into_bytes());
        let v = self.send(
            self.req(Method::POST, &format!("/users/{uid}/playlists/{kind}/cover/upload"))
                .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
                .body(body),
        )?;
        Ok(v.pointer("/cover/uri").and_then(|u| u.as_str()).unwrap_or("").to_string())
    }

    pub fn lyrics(&self, track_id: &str) -> Res<Lyrics> {
        let mut out = Lyrics::default();
        for format in ["LRC", "TEXT"] {
            let ts = now().as_secs().to_string();
            let sg = sign(&format!("{track_id}{ts}"))?;
            let v = match self.get(&format!("/tracks/{track_id}/lyrics"), &[("format", format), ("timeStamp", &ts), ("sign", &sg)]) {
                Ok(v) => v,
                // Синхронного текста нет у многих треков — тогда берётся обычный.
                Err(_) if format == "LRC" => continue,
                // 404 — у трека нет текста вообще: это не ошибка, показываем пустой.
                Err(e) if e.starts_with("HTTP 404") => return Ok(out),
                Err(e) => return Err(e),
            };
            let url = v.get("downloadUrl").and_then(|u| u.as_str()).ok_or("нет ссылки на текст")?;
            let (status, text) = self.send_text(self.http.get(url))?;
            if !status.is_success() {
                return Err(format!("текст: HTTP {status}"));
            }
            out.writers = v
                .get("writers")
                .and_then(|w| w.as_array())
                .map(|w| w.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            out.source = v.pointer("/major/prettyName").and_then(|m| m.as_str()).unwrap_or("").to_string();
            if format == "LRC" {
                out.synced = parse_lrc(&text);
                if !out.synced.is_empty() {
                    return Ok(out);
                }
            } else {
                out.text = text.lines().map(|l| l.trim_end().to_string()).collect();
            }
        }
        Ok(out)
    }

    // ---------- аудио ----------

    /// Прямая ссылка на аудио: сначала download-info (mp3 до 320), потом get-file-info.
    pub fn stream_url(&self, track_id: &str) -> Res<String> {
        match self.stream_url_legacy(track_id) {
            Ok(u) => Ok(u),
            Err(e1) => self
                .file_info(track_id, "nq", "mp3,aac,he-aac")
                .map(|(_, u)| u)
                .map_err(|e2| format!("download-info: {e1}; get-file-info: {e2}")),
        }
    }

    /// Lossless без шифрования есть только как FLAC в MP4. None — у трека нет FLAC.
    pub fn flac_url(&self, track_id: &str) -> Res<Option<String>> {
        let (codec, url) = self.file_info(track_id, "lossless", "flac-mp4,flac")?;
        Ok(codec.starts_with("flac").then_some(url))
    }

    fn stream_url_legacy(&self, track_id: &str) -> Res<String> {
        let v = self.get(&format!("/tracks/{track_id}/download-info"), &[])?;
        let arr = v.as_array().ok_or("пустой download-info")?;
        let key = |x: &&Value| {
            (
                !x.get("preview").and_then(|p| p.as_bool()).unwrap_or(false),
                x.get("bitrateInKbps").and_then(|b| b.as_u64()).unwrap_or(0),
            )
        };
let mp3 = || arr.iter().filter(|x| x.get("codec").and_then(|c| c.as_str()) == Some("mp3"));
        let full = |x: &&Value| !x.get("preview").and_then(|p| p.as_bool()).unwrap_or(false);
        let bitrate = |x: &&Value| x.get("bitrateInKbps").and_then(|b| b.as_u64()).unwrap_or(0);
        let pick = match QUALITY.load(std::sync::atomic::Ordering::Relaxed) {
            2 => mp3().filter(full).min_by_key(bitrate),
            1 => mp3().filter(full).filter(|x| bitrate(x) <= 192).max_by_key(bitrate),
            _ => None,
        }
        .or_else(|| mp3().max_by_key(key))
        .or_else(|| arr.iter().max_by_key(key))
        .ok_or("нет вариантов загрузки")?;
        let info_url = pick.get("downloadInfoUrl").and_then(|u| u.as_str()).ok_or("нет downloadInfoUrl")?;
        if !reqwest::Url::parse(info_url).is_ok_and(|u| token_allowed(&u)) {
            return Err("download-info: адрес не на сервере Яндекса".into());
        }
        let (status, xml) = self.send_text(self.req(Method::GET, info_url))?;
        if !status.is_success() {
            return Err(format!("download-info HTTP {status}"));
        }
        let host = xml_tag(&xml, "host")?;
        let path = xml_tag(&xml, "path")?;
        let ts = xml_tag(&xml, "ts")?;
        let sg = xml_tag(&xml, "s")?;
        let digest = Md5::digest(format!("{MP3_SALT}{}{sg}", path.get(1..).unwrap_or("")).as_bytes());
        let hash: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        Ok(format!("https://{host}/get-mp3/{hash}/{ts}{path}"))
    }

    /// Кодек и ссылка из get-file-info.
    fn file_info(&self, track_id: &str, quality: &str, codecs: &str) -> Res<(String, String)> {
        let ts = now().as_secs().to_string();
        let transports = "raw";
        let mut sg = sign(&format!("{ts}{track_id}{quality}{codecs}{transports}").replace(',', ""))?;
        sg.pop();
        let v = self.get(
            "/get-file-info",
            &[
                ("ts", ts.as_str()),
                ("trackId", track_id),
                ("quality", quality),
                ("codecs", codecs),
                ("transports", transports),
                ("sign", sg.as_str()),
            ],
        )?;
        let di = v.get("downloadInfo").ok_or("нет downloadInfo")?;
        let url = di
            .get("url")
            .and_then(|u| u.as_str())
            .or_else(|| di.get("urls")?.get(0)?.as_str())
            .ok_or("нет url в downloadInfo")?;
        Ok((str_of(di, "codec"), url.to_string()))
    }

    /// Скачать файл потоком на диск, без чтения целиком в память. Ссылки уже подписаны — без токена.
    pub fn download_to(&self, url: &str, path: &std::path::Path) -> Res<u64> {
        let mut resp = self.http.get(url).send().map_err(|e| format!("сеть: {}", crate::config::chain(&e)))?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        let expected = resp.content_length();
        let mut f = std::fs::File::create(path).map_err(|e| format!("файл: {e}"))?;
        let n = std::io::copy(&mut resp, &mut f).map_err(|e| format!("загрузка прервалась: {e}"))?;
        f.sync_all().map_err(|e| format!("файл: {e}"))?;
        if expected.is_some_and(|e| e != n) {
            return Err(format!("загрузка оборвалась: {n} из {} байт", expected.unwrap_or(0)));
        }
        Ok(n)
    }

    /// Скачать файл целиком (аудио, обложка). Ссылки уже подписаны — без токена.
    /// Ответ без чтения тела: трек дальше качается по кусочкам.
    pub fn open(&self, url: &str) -> Res<reqwest::blocking::Response> {
        let resp = self.http.get(url).send().map_err(|e| format!("сеть: {}", crate::config::chain(&e)))?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        Ok(resp)
    }

    pub fn fetch_bytes(&self, url: &str) -> Res<Vec<u8>> {
        let resp = self.http.get(url).send().map_err(|e| format!("сеть: {}", crate::config::chain(&e)))?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        resp.bytes().map(|b| b.to_vec()).map_err(|e| format!("сеть: {}", crate::config::chain(&e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lrc() {
        let l = parse_lrc("[00:04.64] Uh, check\n[00:05.99] \n[01:15.26] Are you listening?\n[01:20.00]");
        assert_eq!(l.len(), 3);
        assert!((l[2].from - 75.26).abs() < 0.01);
        assert_eq!(l[2].to, Some(80.0));
        assert_eq!(l[0].to, Some(5.99));
        assert_eq!(l[1].text, "");
    }

    #[test]
    fn pic_size() {
        assert_eq!(pic("avatars.yandex.net/a/%%", 200), "https://avatars.yandex.net/a/200x200");
    }
}
