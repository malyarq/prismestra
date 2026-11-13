//! Web form adapter for controls already owned and validated by the native editor.
use super::*;
use serde_json::{Value, json};

const MAX_ROWS: usize = 512;
const MAX_TEXT: usize = 4096;
const REVERT_SCHEDULE: i32 = 408;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Text,
    Secret,
    Select,
    Checkbox,
    Action,
    Note,
}

fn allowed(app: &App, id: i32) -> bool {
    match app.page {
        Page::Connections => (1800..=1821).contains(&id) && app.connection_visible(id),
        Page::Sources => (1100..=1153).contains(&id) && app.source_visible(id),
        Page::Schedule => (401..=407).contains(&id),
        _ => aux_allowed(app.page, id),
    }
}

fn aux_allowed(page: Page, id: i32) -> bool {
    match id {
        E_ADD => matches!(
            page,
            Page::Devices | Page::Ambient | Page::Lcd | Page::Temperatures
        ),
        E_REMOVE => matches!(page, Page::Devices | Page::Ambient | Page::Temperatures),
        E_TEST => matches!(page, Page::Ambient | Page::Temperatures),
        E_PRESET | E_LED_COUNT => page == Page::Devices,
        E_ROUTE_DEVICE | E_ROUTE_SOURCE => page == Page::Ambient,
        _ => false,
    }
}

fn control(app: &App, id: i32) -> Option<(HWND, Kind)> {
    if !allowed(app, id) {
        return None;
    }
    let hwnd = *app.widgets.get(&id)?;
    if hwnd.is_null() {
        return None;
    }
    let mut name = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, name.as_mut_ptr(), name.len() as i32) };
    let class = String::from_utf16_lossy(&name[..len.max(0) as usize]);
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
    let kind = match class.to_ascii_uppercase().as_str() {
        "EDIT" => {
            if secret(hwnd, style) {
                Kind::Secret
            } else if style & ES_READONLY as u32 != 0 {
                Kind::Note
            } else {
                Kind::Text
            }
        }
        "COMBOBOX" | "LISTBOX" => Kind::Select,
        "BUTTON" => match style & BS_TYPEMASK as u32 {
            n if n == BS_CHECKBOX as u32 || n == BS_AUTOCHECKBOX as u32 => Kind::Checkbox,
            n if n == BS_PUSHBUTTON as u32 || n == BS_DEFPUSHBUTTON as u32 => Kind::Action,
            _ => return None,
        },
        "STATIC" if matches!(id, 1820 | 1112) => Kind::Note,
        _ => return None,
    };
    Some((hwnd, kind))
}

fn secret(hwnd: HWND, style: u32) -> bool {
    style & ES_PASSWORD as u32 != 0 || unsafe { SendMessageW(hwnd, EM_GETPASSWORDCHAR, 0, 0) } != 0
}

fn text(hwnd: HWND) -> String {
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
    // Tokens may be masked dynamically using EM_SETPASSWORDCHAR.
    if secret(hwnd, style) {
        return String::new();
    }
    let mut buffer = [0u16; MAX_TEXT + 1];
    let len = unsafe { GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
    String::from_utf16_lossy(&buffer[..(len.max(0) as usize).min(MAX_TEXT)])
}

fn label(app: &App, id: i32, hwnd: HWND, kind: Kind) -> String {
    if kind == Kind::Action || kind == Kind::Checkbox {
        return text(hwnd);
    }
    let label_id = match id {
        1803 => Some(1816),
        1801 | 1806 | 1808 | 1810 => Some(id - 1),
        1120..=1125 => Some(id + 10),
        1102 => Some(1113),
        1103 => Some(1114),
        1108 => Some(1115),
        1109 => Some(1116),
        _ => None,
    };
    if let Some(label) = label_id
        .and_then(|id| app.widgets.get(&id))
        .map(|hwnd| text(*hwnd))
        && !label.is_empty()
    {
        return label;
    }
    match id {
        1100 => "Раздел датчиков",
        1101 => "Датчик или результат поиска",
        TIME_PC => "Включение ПК",
        TIME_DOOR => "Включение ленты у двери",
        TIME_OFF => "Выключение подсветки",
        TIMEZONE => "Часовой пояс",
        E_PRESET => "Семейство",
        E_LED_COUNT => "Число LED",
        E_ROUTE_DEVICE => "Устройство подсветки",
        E_ROUTE_SOURCE => "Источник экрана",
        _ => "Параметр",
    }
    .into()
}

fn listbox(hwnd: HWND) -> bool {
    let mut name = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, name.as_mut_ptr(), name.len() as i32) };
    String::from_utf16_lossy(&name[..len.max(0) as usize]).eq_ignore_ascii_case("LISTBOX")
}

fn row_count(hwnd: HWND, list: bool) -> usize {
    let count = unsafe { SendMessageW(hwnd, if list { LB_GETCOUNT } else { CB_GETCOUNT }, 0, 0) };
    usize::try_from(count).unwrap_or(0).min(MAX_ROWS)
}

fn choices(hwnd: HWND) -> Vec<Value> {
    let list = listbox(hwnd);
    (0..row_count(hwnd, list)).filter_map(|index| {
        let len = unsafe { SendMessageW(hwnd, if list { LB_GETTEXTLEN } else { CB_GETLBTEXTLEN }, index, 0) };
        // LB_GETTEXT/CB_GETLBTEXT have no capacity argument; query before reading.
        let len = usize::try_from(len).ok().filter(|len| *len <= MAX_TEXT)?;
        let mut buffer = vec![0u16; len + 1];
        let read = unsafe { SendMessageW(hwnd, if list { LB_GETTEXT } else { CB_GETLBTEXT }, index, buffer.as_mut_ptr() as isize) };
        if read < 0 { return None; }
        Some(json!({"label":String::from_utf16_lossy(&buffer[..(read as usize).min(len)]),"value":index.to_string()}))
    }).collect()
}

pub(super) fn snapshot(app: &App) -> Option<Value> {
    let (page, ids) = match app.page {
        Page::Connections => ("connections", 1800..=1821),
        Page::Sources => ("sources", 1100..=1153),
        Page::Schedule => ("schedule", 401..=407),
        _ => return None,
    };
    let mut form = form_snapshot(app, page, ids);
    if app.page == Page::Schedule {
        let dirty = schedule_dirty(app);
        form["dirty"] = json!(dirty);
        form["actions"]
            .as_array_mut()
            .expect("form action array")
            .push(json!({
                "id":REVERT_SCHEDULE,"label":"Отменить изменения","enabled":dirty
            }));
    }
    Some(form)
}

pub(super) fn aux_snapshot(app: &App) -> Option<Value> {
    let page = match app.page {
        Page::Devices => "devices",
        Page::Ambient => "ambient",
        Page::Lcd => "lcd",
        Page::Temperatures => "temperatures",
        _ => return None,
    };
    Some(form_snapshot(
        app,
        page,
        [
            E_ADD,
            E_REMOVE,
            E_TEST,
            E_PRESET,
            E_LED_COUNT,
            E_ROUTE_DEVICE,
            E_ROUTE_SOURCE,
        ],
    ))
}

fn form_snapshot(app: &App, page: &str, ids: impl IntoIterator<Item = i32>) -> Value {
    let mut fields = Vec::new();
    let mut actions = Vec::new();
    let mut notes = Vec::new();
    for id in ids {
        let Some((hwnd, kind)) = control(app, id) else {
            continue;
        };
        let enabled = unsafe { IsWindowEnabled(hwnd) } != 0;
        if kind == Kind::Note {
            let note = text(hwnd);
            if !note.is_empty() {
                notes.push(note);
            }
        } else if kind == Kind::Action {
            actions.push(json!({"id":id,"label":label(app,id,hwnd,kind),"enabled":enabled}));
        } else {
            let value = match kind {
                Kind::Secret => String::new(),
                Kind::Checkbox => (unsafe { SendMessageW(hwnd, BM_GETCHECK, 0, 0) }
                    == BST_CHECKED as isize)
                    .to_string(),
                Kind::Select => {
                    let index = unsafe {
                        SendMessageW(
                            hwnd,
                            if listbox(hwnd) {
                                LB_GETCURSEL
                            } else {
                                CB_GETCURSEL
                            },
                            0,
                            0,
                        )
                    };
                    if index < 0 {
                        String::new()
                    } else {
                        index.to_string()
                    }
                }
                _ => text(hwnd),
            };
            let kind_name = match kind {
                Kind::Text => "text",
                Kind::Secret => "secret",
                Kind::Checkbox => "checkbox",
                _ => "select",
            };
            let options = if kind == Kind::Select {
                choices(hwnd)
            } else {
                Vec::new()
            };
            fields.push(json!({"id":id,"label":label(app,id,hwnd,kind),"kind":kind_name,"value":value,"choices":options,"enabled":enabled}));
        }
    }
    json!({"page":page,"fields":fields,"actions":actions,"notes":notes})
}

fn validate_text(value: &str) -> Result<(), String> {
    if value.len() > MAX_TEXT || value.contains('\0') {
        return Err("Значение слишком длинное или содержит нулевой символ".into());
    }
    Ok(())
}

fn choice_index(value: &str, count: usize) -> Result<usize, String> {
    let index = value
        .parse::<usize>()
        .map_err(|_| "Неверный пункт списка")?;
    if index >= count || value != index.to_string() {
        return Err("Пункт списка недоступен".into());
    }
    Ok(index)
}

fn checked(value: &str) -> Result<usize, String> {
    match value {
        "true" => Ok(BST_CHECKED as usize),
        "false" => Ok(BST_UNCHECKED as usize),
        _ => Err("Ожидается true или false".into()),
    }
}

pub(super) fn set_field(app: &mut App, id: i32, value: &str) -> Result<(), String> {
    validate_text(value)?;
    let (hwnd, kind) = control(app, id).ok_or("Поле недоступно на этой странице")?;
    if unsafe { IsWindowEnabled(hwnd) } == 0 {
        return Err("Поле отключено".into());
    }
    let notice = match kind {
        Kind::Text | Kind::Secret => {
            if unsafe { SetWindowTextW(hwnd, wide(value).as_ptr()) } == 0 {
                return Err(last("Изменение поля"));
            }
            EN_CHANGE as u16
        }
        Kind::Checkbox => {
            unsafe {
                SendMessageW(hwnd, BM_SETCHECK, checked(value)?, 0);
            }
            BN_CLICKED as u16
        }
        Kind::Select => {
            let list = listbox(hwnd);
            let index = choice_index(value, row_count(hwnd, list))?;
            // Use the same exposed choices as the DTO, including the text-size limit.
            if !choices(hwnd)
                .iter()
                .any(|choice| choice["value"].as_str() == Some(value))
            {
                return Err("Пункт списка недоступен".into());
            }
            if unsafe {
                SendMessageW(
                    hwnd,
                    if list { LB_SETCURSEL } else { CB_SETCURSEL },
                    index,
                    0,
                )
            } < 0
            {
                return Err("Не удалось выбрать пункт списка".into());
            }
            if list {
                LBN_SELCHANGE as u16
            } else {
                CBN_SELCHANGE as u16
            }
        }
        _ => return Err("Это не редактируемое поле".into()),
    };
    app.command(id, notice)
}

fn validate_batch(fields: &[(i32, String)]) -> Result<(), String> {
    if fields.len() > 64 {
        return Err("Слишком много полей в одной операции".into());
    }
    let mut ids = std::collections::BTreeSet::new();
    for (id, value) in fields {
        if !ids.insert(*id) {
            return Err("Поле передано повторно".into());
        }
        validate_text(value)?;
    }
    Ok(())
}

pub(super) fn apply_text_fields(app: &mut App, fields: &[(i32, String)]) -> Result<(), String> {
    validate_batch(fields)?;
    for (id, _) in fields {
        let (hwnd, kind) = control(app, *id).ok_or("Поле недоступно на этой странице")?;
        if !matches!(kind, Kind::Text | Kind::Secret) || unsafe { IsWindowEnabled(hwnd) } == 0 {
            return Err("Текстовое поле недоступно".into());
        }
    }
    for (id, value) in fields {
        set_field(app, *id, value)?;
    }
    Ok(())
}

fn schedule_values_dirty(
    applied: &schedule::Config,
    enabled: bool,
    paused: bool,
    times: [&str; 3],
    timezone: &str,
) -> bool {
    enabled != applied.enabled
        || paused != applied.paused
        || timezone != applied.timezone
        || times
            .into_iter()
            .zip(applied.times)
            .any(|(text, minute)| text != format!("{:02}:{:02}", minute / 60, minute % 60))
}

fn schedule_dirty(app: &App) -> bool {
    if ![
        SCHED_ON,
        SCHED_PAUSE,
        TIME_PC,
        TIME_DOOR,
        TIME_OFF,
        TIMEZONE,
    ]
    .iter()
    .all(|id| app.widgets.contains_key(id))
    {
        return false;
    }
    let times = [TIME_PC, TIME_DOOR, TIME_OFF].map(|id| text(app.h(id)));
    schedule_values_dirty(
        &app.runtime.project().schedule,
        app.checked(SCHED_ON),
        app.checked(SCHED_PAUSE),
        times.each_ref().map(String::as_str),
        &text(app.h(TIMEZONE)),
    )
}

pub(super) fn guard_close(app: &App) -> Result<(), String> {
    if app.page == Page::Schedule && schedule_dirty(app) {
        return Err("Расписание изменено. Примените его или нажмите «Отменить изменения».".into());
    }
    Ok(())
}

pub(super) fn action(state: &State, id: i32) -> Result<(), String> {
    if id == REVERT_SCHEDULE {
        let mut slot = state.app.try_borrow_mut().map_err(|_| "Редактор занят")?;
        let app = slot.as_mut().ok_or("Нет сессии редактора")?;
        if app.page != Page::Schedule || !schedule_dirty(app) {
            return Err("Нет изменений расписания для отмены".into());
        }
        app.sync();
        return Ok(());
    }
    {
        let slot = state.app.try_borrow().map_err(|_| "Редактор занят")?;
        let app = slot.as_ref().ok_or("Нет сессии редактора")?;
        let (hwnd, kind) = control(app, id).ok_or("Действие недоступно на этой странице")?;
        if kind != Kind::Action || unsafe { IsWindowEnabled(hwnd) } == 0 {
            return Err("Действие отключено".into());
        }
    }
    if id == sources::MODULE_SELECT {
        return modal_command(state.editor_window.get(), state, id);
    }
    let mut slot = state.app.try_borrow_mut().map_err(|_| "Редактор занят")?;
    slot.as_mut()
        .ok_or("Нет сессии редактора")?
        .command(id, BN_CLICKED as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn auxiliary_controls_match_native_page_visibility() {
        for page in [Page::Devices, Page::Ambient, Page::Lcd, Page::Temperatures] {
            assert!(aux_allowed(page, E_ADD));
            assert_eq!(aux_allowed(page, E_REMOVE), page != Page::Lcd);
            assert_eq!(
                aux_allowed(page, E_TEST),
                matches!(page, Page::Ambient | Page::Temperatures)
            );
            for id in [E_PRESET, E_LED_COUNT] {
                assert_eq!(aux_allowed(page, id), page == Page::Devices);
            }
            for id in [E_ROUTE_DEVICE, E_ROUTE_SOURCE] {
                assert_eq!(aux_allowed(page, id), page == Page::Ambient);
            }
            for id in [E_LIST, E_VALUE, E_CHOICE, E_SET, E_FILE, E_NEW_LABEL] {
                assert!(!aux_allowed(page, id));
            }
        }
        for page in [
            Page::Scene,
            Page::Connections,
            Page::Sources,
            Page::Schedule,
        ] {
            for id in [
                E_ADD,
                E_REMOVE,
                E_TEST,
                E_PRESET,
                E_LED_COUNT,
                E_ROUTE_DEVICE,
                E_ROUTE_SOURCE,
            ] {
                assert!(!aux_allowed(page, id));
            }
        }
    }

    #[test]
    fn batch_preflight_rejects_duplicates_oversize_and_nul() {
        let fields = (0..64).map(|id| (id, "value".into())).collect::<Vec<_>>();
        assert!(validate_batch(&fields).is_ok());
        let mut many = fields;
        many.push((64, "value".into()));
        assert!(validate_batch(&many).is_err());
        assert!(validate_batch(&[(403, "10:00".into()), (403, "11:00".into())]).is_err());
        assert!(validate_batch(&[(403, "x".repeat(MAX_TEXT + 1))]).is_err());
        assert!(validate_batch(&[(403, "a\0b".into())]).is_err());
    }

    #[test]
    fn schedule_dirty_tracks_all_unapplied_native_values() {
        let applied = schedule::Config::default();
        let times = ["10:00", "17:00", "02:00"];
        assert!(!schedule_values_dirty(
            &applied,
            false,
            false,
            times,
            "Europe/Moscow"
        ));
        assert!(schedule_values_dirty(
            &applied,
            true,
            false,
            times,
            "Europe/Moscow"
        ));
        assert!(schedule_values_dirty(
            &applied,
            false,
            true,
            times,
            "Europe/Moscow"
        ));
        assert!(schedule_values_dirty(&applied, false, false, times, "UTC"));
        for index in 0..3 {
            for value in ["11:11", "bad", ""] {
                let mut changed = times;
                changed[index] = value;
                assert!(schedule_values_dirty(
                    &applied,
                    false,
                    false,
                    changed,
                    "Europe/Moscow"
                ));
            }
        }
        let applied = schedule::Config {
            enabled: true,
            paused: true,
            timezone: "UTC".into(),
            times: [60, 120, 180],
        };
        assert!(!schedule_values_dirty(
            &applied,
            true,
            true,
            ["01:00", "02:00", "03:00"],
            "UTC"
        ));
    }

    #[test]
    fn input_limits_and_choice_indices_are_strict() {
        assert!(validate_text(&"x".repeat(MAX_TEXT)).is_ok());
        assert!(validate_text(&"x".repeat(MAX_TEXT + 1)).is_err());
        assert!(validate_text("a\0b").is_err());
        assert_eq!(choice_index("511", MAX_ROWS).unwrap(), 511);
        for value in ["512", "-1", "+1", "01", " 1", "", "1.0"] {
            assert!(choice_index(value, MAX_ROWS).is_err(), "{value}");
        }
        assert_eq!(checked("true").unwrap(), BST_CHECKED as usize);
        assert_eq!(checked("false").unwrap(), BST_UNCHECKED as usize);
        for value in ["1", "0", "True", "false ", ""] {
            assert!(checked(value).is_err());
        }
    }
}
