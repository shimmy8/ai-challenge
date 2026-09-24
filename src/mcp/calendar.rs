use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, TimeZone, Utc};
use reqwest::{Client, Method, StatusCode, Url};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{env, fs, path::Path};
use uuid::Uuid;

pub(crate) const DEFAULT_CALDAV_ENDPOINT: &str = "https://caldav.yandex.ru";

fn calendar_log(message: impl AsRef<str>) {
    eprintln!("MCP calendar: {}", message.as_ref());
}

/// Load CalDAV variables for the MCP server from a local dotenv file.
/// Explicit process variables take precedence over values from the file.
pub(crate) fn load_mcp_env_file(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let contents = fs::read_to_string(path)
        .with_context(|| format!("не удалось прочитать {}", path.display()))?;
    for (line_number, line) in contents.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, raw_value)) = line.split_once('=') else {
            bail!(
                "некорректная строка {} в {}",
                line_number + 1,
                path.display()
            );
        };
        let key = key.trim();
        anyhow::ensure!(
            (key.starts_with("YANDEX_CALDAV_")
                || matches!(key, "TELEGRAM_BOT_TOKEN" | "TELEGRAM_CHAT_ID"))
                && key.chars().all(|character| {
                    character.is_ascii_uppercase() || character == '_' || character.is_ascii_digit()
                }),
            "недопустимое имя переменной в {}:{}",
            path.display(),
            line_number + 1
        );
        if env::var_os(key).is_none() {
            env::set_var(key, parse_env_value(raw_value.trim()));
        }
    }
    Ok(true)
}

fn parse_env_value(value: &str) -> String {
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        value[1..value.len() - 1].to_owned()
    } else {
        value.split_once(" #").map_or_else(
            || value.to_owned(),
            |(value, _)| value.trim_end().to_owned(),
        )
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(crate) struct CalendarEventRequest {
    /// Human-readable event title.
    pub(crate) title: String,
    /// RFC 3339 start timestamp including an explicit offset.
    pub(crate) start_at: String,
    /// RFC 3339 end timestamp including an explicit offset.
    pub(crate) end_at: String,
    /// Optional event description.
    #[serde(default)]
    pub(crate) description: Option<String>,
    /// Optional event location.
    #[serde(default)]
    pub(crate) location: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub(crate) struct CalendarEventResult {
    pub(crate) event_id: String,
    pub(crate) title: String,
    pub(crate) start_at: String,
    pub(crate) end_at: String,
    pub(crate) calendar: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub(crate) struct CalendarDigestEvent {
    pub(crate) title: String,
    pub(crate) start_at: String,
    pub(crate) end_at: Option<String>,
    pub(crate) all_day: bool,
    pub(crate) location: Option<String>,
}

#[derive(Clone)]
pub(crate) struct CalendarSettings {
    pub(crate) username: String,
    pub(crate) password: String,
    pub(crate) calendar_name: Option<String>,
    pub(crate) endpoint: Url,
}

impl CalendarSettings {
    pub(crate) fn from_env() -> Result<Self> {
        let username = env::var("YANDEX_CALDAV_USERNAME")
            .context("не задана переменная YANDEX_CALDAV_USERNAME")?;
        let password = env::var("YANDEX_CALDAV_PASSWORD")
            .context("не задана переменная YANDEX_CALDAV_PASSWORD")?;
        let endpoint = env::var("YANDEX_CALDAV_ENDPOINT")
            .unwrap_or_else(|_| DEFAULT_CALDAV_ENDPOINT.to_owned());
        Ok(Self {
            username,
            password,
            calendar_name: env::var("YANDEX_CALDAV_CALENDAR").ok(),
            endpoint: Url::parse(&endpoint).context("адрес CalDAV некорректен")?,
        })
    }
}

#[derive(Debug, Clone)]
struct CalendarCollection {
    href: Url,
    display_name: String,
}

#[derive(Clone)]
pub(crate) struct CalDavClient {
    http: Client,
    settings: CalendarSettings,
}

impl CalDavClient {
    pub(crate) fn from_env() -> Result<Self> {
        Ok(Self {
            http: Client::builder()
                .user_agent("fox-llm-calendar/0.1")
                .build()
                .context("не удалось создать CalDAV-клиент")?,
            settings: CalendarSettings::from_env()?,
        })
    }

    pub(crate) async fn create_event(
        &self,
        request: &CalendarEventRequest,
    ) -> Result<CalendarEventResult> {
        calendar_log("начинаю создание события");
        let (start, end) = validate_event(request)?;
        let collection = self.select_calendar().await?;
        calendar_log(format!(
            "выбран календарь «{}», выполняю PUT события",
            collection.display_name
        ));
        let id = Uuid::new_v4().to_string();
        let event_url = collection.href.join(&format!("{id}.ics"))?;
        let body = build_icalendar(&id, request, start, end);
        let response = self
            .http
            .request(Method::PUT, event_url.clone())
            .basic_auth(&self.settings.username, Some(&self.settings.password))
            .header("Content-Type", "text/calendar; charset=utf-8")
            .header("If-None-Match", "*")
            .body(body)
            .send()
            .await
            .context("не удалось отправить событие в Яндекс Календарь")?;
        let status = response.status();
        calendar_log(format!("PUT события завершён со статусом {status}"));
        if !status.is_success() {
            let _ = response.text().await;
            bail!("Яндекс Календарь отклонил создание события ({status})");
        }
        Ok(CalendarEventResult {
            event_id: id,
            title: request.title.trim().to_owned(),
            start_at: start.to_rfc3339(),
            end_at: end.to_rfc3339(),
            calendar: collection.display_name,
        })
    }

    pub(crate) async fn list_events(
        &self,
        from: DateTime<FixedOffset>,
        to: DateTime<FixedOffset>,
    ) -> Result<Vec<CalendarDigestEvent>> {
        anyhow::ensure!(to > from, "интервал календаря должен быть положительным");
        let collection = self.select_calendar().await?;
        let body = format!(
            "<c:calendar-query xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\"><d:prop><d:getetag/><c:calendar-data/></d:prop><c:filter><c:comp-filter name=\"VCALENDAR\"><c:comp-filter name=\"VEVENT\"><c:time-range start=\"{}\" end=\"{}\"/></c:comp-filter></c:comp-filter></c:filter></c:calendar-query>",
            from.with_timezone(&Utc).format("%Y%m%dT%H%M%SZ"),
            to.with_timezone(&Utc).format("%Y%m%dT%H%M%SZ"),
        );
        let response = self
            .http
            .request(Method::from_bytes(b"REPORT")?, collection.href)
            .basic_auth(&self.settings.username, Some(&self.settings.password))
            .header("Depth", "1")
            .header("Content-Type", "application/xml; charset=utf-8")
            .body(body)
            .send()
            .await
            .context("не удалось прочитать события Яндекс Календаря")?;
        let status = response.status();
        if status != StatusCode::MULTI_STATUS && !status.is_success() {
            let _ = response.text().await;
            bail!("CalDAV чтение календаря завершилось ошибкой ({status})");
        }
        let xml = response
            .text()
            .await
            .context("не удалось прочитать ответ календаря")?;
        let mut events = Vec::new();
        for data in xml_texts(&xml, "calendar-data") {
            for event in parse_digest_events(&data) {
                if event_overlaps(&event, from, to) {
                    events.push(event);
                }
            }
        }
        events.sort_by(|left, right| left.start_at.cmp(&right.start_at));
        Ok(events)
    }

    async fn select_calendar(&self) -> Result<CalendarCollection> {
        calendar_log("CalDAV discovery: current-user-principal");
        let principal = self
            .propfind_href(&self.settings.endpoint, 0, "current-user-principal")
            .await
            .context("не удалось определить CalDAV principal")?;
        calendar_log("CalDAV discovery: calendar-home-set");
        let home = self
            .propfind_href(&principal, 0, "calendar-home-set")
            .await
            .context("не удалось определить домашний каталог календарей")?;
        let xml = self.propfind(&home, 1).await?;
        let mut collections = parse_calendar_collections(&home, &xml)?;
        collections.retain(|collection| !collection.display_name.trim().is_empty());
        calendar_log(format!(
            "CalDAV discovery: найдено календарей {}",
            collections.len(),
        ));
        choose_calendar(&collections, self.settings.calendar_name.as_deref())
    }

    async fn propfind_href(&self, url: &Url, depth: u8, property: &str) -> Result<Url> {
        let xml = self.propfind_with_body(
            url,
            depth,
            &format!(
                "<d:propfind xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\"><d:prop><d:{property}/><c:{property}/></d:prop></d:propfind>"
            ),
        )
        .await?;
        // A PROPFIND response contains its resource href first and the
        // requested property's href inside the property value. The latter is
        // the final href and is the one needed for principal/home discovery.
        let href = xml_texts(&xml, "href")
            .into_iter()
            .last()
            .ok_or_else(|| anyhow!("CalDAV не вернул href для {property}"))?;
        url.join(href.trim())
            .context("CalDAV вернул некорректный href")
    }

    async fn propfind(&self, url: &Url, depth: u8) -> Result<String> {
        self.propfind_with_body(
            url,
            depth,
            "<d:propfind xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\"><d:prop><d:displayname/><d:resourcetype/><d:current-user-privilege-set/><c:calendar-description/></d:prop></d:propfind>",
        )
        .await
    }

    async fn propfind_with_body(&self, url: &Url, depth: u8, body: &str) -> Result<String> {
        calendar_log(format!(
            "PROPFIND depth={depth} host={}",
            url.host_str().unwrap_or("unknown")
        ));
        let response = self
            .http
            .request(Method::from_bytes(b"PROPFIND")?, url.clone())
            .basic_auth(&self.settings.username, Some(&self.settings.password))
            .header("Depth", depth.to_string())
            .header("Content-Type", "application/xml; charset=utf-8")
            .body(body.to_owned())
            .send()
            .await
            .context("не удалось выполнить CalDAV discovery")?;
        let status = response.status();
        calendar_log(format!("PROPFIND завершён со статусом {status}"));
        if status != StatusCode::MULTI_STATUS && !status.is_success() {
            let _ = response.text().await;
            bail!("CalDAV discovery завершился с ошибкой ({status})");
        }
        response
            .text()
            .await
            .context("не удалось прочитать ответ CalDAV discovery")
    }
}

fn choose_calendar(
    collections: &[CalendarCollection],
    requested_name: Option<&str>,
) -> Result<CalendarCollection> {
    if let Some(name) = requested_name {
        let matches = collections
            .iter()
            .filter(|collection| collection.display_name == name)
            .cloned()
            .collect::<Vec<_>>();
        return match matches.as_slice() {
            [collection] => Ok(collection.clone()),
            [] => bail!("CalDAV-календарь «{name}» не найден"),
            _ => bail!("имя CalDAV-календаря «{name}» неоднозначно"),
        };
    }
    match collections {
        [collection] => Ok(collection.clone()),
        [] => bail!("не найден доступный для записи календарь CalDAV"),
        _ => {
            let names = collections
                .iter()
                .map(|collection| collection.display_name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            bail!("найдено несколько календарей ({names}); задайте YANDEX_CALDAV_CALENDAR")
        }
    }
}

fn validate_event(
    request: &CalendarEventRequest,
) -> Result<(DateTime<FixedOffset>, DateTime<FixedOffset>)> {
    anyhow::ensure!(
        !request.title.trim().is_empty(),
        "название события не может быть пустым"
    );
    let start = parse_timestamp(&request.start_at, "начала")?;
    let end = parse_timestamp(&request.end_at, "окончания")?;
    anyhow::ensure!(end > start, "окончание события должно быть позже начала");
    Ok((start, end))
}

fn parse_timestamp(value: &str, label: &str) -> Result<DateTime<FixedOffset>> {
    let parsed = DateTime::parse_from_rfc3339(value)
        .with_context(|| format!("время {label} должно быть RFC 3339 со смещением"))?;
    anyhow::ensure!(
        value.contains('+') || value.ends_with('Z'),
        "время {label} должно содержать смещение"
    );
    Ok(parsed)
}

fn parse_digest_event(ics: &str) -> Result<CalendarDigestEvent> {
    let lines = unfold_icalendar(ics);
    let value = |name: &str| {
        lines.iter().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.split(';').next() == Some(name)).then(|| value.to_owned())
        })
    };
    let start_line = lines
        .iter()
        .find(|line| line.starts_with("DTSTART"))
        .ok_or_else(|| anyhow!("событие не содержит DTSTART"))?;
    let all_day = start_line.split(';').any(|part| part == "VALUE=DATE");
    let start_raw = start_line
        .split_once(':')
        .map(|(_, value)| value)
        .unwrap_or_default();
    let start = parse_ical_time(start_raw, all_day)?;
    let end = value("DTEND")
        .map(|raw| parse_ical_time(&raw, all_day))
        .transpose()?;
    Ok(CalendarDigestEvent {
        title: value("SUMMARY").unwrap_or_else(|| "Без названия".into()),
        start_at: start,
        end_at: end,
        all_day,
        location: value("LOCATION").filter(|value| !value.trim().is_empty()),
    })
}

fn parse_digest_events(ics: &str) -> Vec<CalendarDigestEvent> {
    let unfolded = ics.replace("\r\n", "\n").replace('\r', "\n");
    unfolded
        .split("BEGIN:VEVENT")
        .skip(1)
        .filter_map(|part| {
            let body = part.split_once("END:VEVENT")?.0;
            parse_digest_event(&format!("BEGIN:VEVENT\n{body}END:VEVENT")).ok()
        })
        .collect()
}

fn unfold_icalendar(value: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in value.replace("\r\n", "\n").replace('\r', "\n").lines() {
        if raw.starts_with(' ') || raw.starts_with('\t') {
            if let Some(last) = lines.last_mut() {
                last.push_str(raw.trim_start());
            }
        } else {
            lines.push(raw.to_owned());
        }
    }
    lines
}

fn parse_ical_time(value: &str, all_day: bool) -> Result<String> {
    if all_day {
        return Ok(NaiveDate::parse_from_str(value, "%Y%m%d")?
            .format("%Y-%m-%d")
            .to_string());
    }
    let is_utc = value.ends_with('Z');
    let value = value.trim_end_matches('Z');
    let parsed = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S")?;
    // Yandex returns DTSTART;TZID=Europe/Moscow without a trailing Z. Such
    // values are local calendar times, not UTC timestamps.
    let offset = if is_utc {
        FixedOffset::east_opt(0).unwrap()
    } else {
        FixedOffset::east_opt(3 * 60 * 60).unwrap()
    };
    Ok(offset
        .from_local_datetime(&parsed)
        .single()
        .expect("fixed offset has one local time")
        .to_rfc3339())
}

fn event_overlaps(
    event: &CalendarDigestEvent,
    from: DateTime<FixedOffset>,
    to: DateTime<FixedOffset>,
) -> bool {
    if event.all_day {
        let Ok(start) = NaiveDate::parse_from_str(&event.start_at, "%Y-%m-%d") else {
            return false;
        };
        let end = event
            .end_at
            .as_deref()
            .and_then(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok())
            .unwrap_or_else(|| start.succ_opt().unwrap_or(start));
        let from_date = from.date_naive();
        let to_date = to.date_naive();
        return start < to_date && end > from_date;
    }
    let Ok(start) = DateTime::parse_from_rfc3339(&event.start_at) else {
        return false;
    };
    let end = event
        .end_at
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .unwrap_or(start + chrono::Duration::minutes(1));
    start < to && end > from
}

pub(crate) fn build_icalendar(
    id: &str,
    request: &CalendarEventRequest,
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
) -> String {
    let now = Utc::now().format("%Y%m%dT%H%M%SZ");
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_owned(),
        "VERSION:2.0".to_owned(),
        "PRODID:-//fox-llm//Yandex Calendar//EN".to_owned(),
        "BEGIN:VEVENT".to_owned(),
        format!("UID:{id}"),
        format!("DTSTAMP:{now}"),
        format!(
            "DTSTART:{}",
            start.with_timezone(&Utc).format("%Y%m%dT%H%M%SZ")
        ),
        format!("DTEND:{}", end.with_timezone(&Utc).format("%Y%m%dT%H%M%SZ")),
        format!("SUMMARY:{}", escape_icalendar_text(request.title.trim())),
    ];
    if let Some(description) = request
        .description
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        lines.push(format!(
            "DESCRIPTION:{}",
            escape_icalendar_text(description.trim())
        ));
    }
    if let Some(location) = request
        .location
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        lines.push(format!(
            "LOCATION:{}",
            escape_icalendar_text(location.trim())
        ));
    }
    lines.extend(["END:VEVENT".to_owned(), "END:VCALENDAR".to_owned()]);
    format!("{}\r\n", lines.join("\r\n"))
}

fn escape_icalendar_text(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\n', "\\n")
        .replace('\r', "")
}

fn parse_calendar_collections(home: &Url, xml: &str) -> Result<Vec<CalendarCollection>> {
    let mut collections = Vec::new();
    for response in xml_blocks(xml, "response") {
        let Some(href) = xml_texts(response, "href").into_iter().next() else {
            continue;
        };
        // Match only the actual empty resource-type `<calendar/>`; broad
        // substring checks also match properties such as calendar-description.
        let has_calendar = has_empty_xml_tag(response, "calendar");
        if !has_calendar {
            continue;
        }
        let display_name = xml_texts(response, "displayname")
            .into_iter()
            .next()
            .unwrap_or_else(|| href.clone());
        collections.push(CalendarCollection {
            href: home.join(href.trim())?,
            display_name: strip_xml_text(&display_name),
        });
    }
    Ok(collections)
}

fn has_empty_xml_tag(xml: &str, local_name: &str) -> bool {
    let mut cursor = 0;
    while let Some(relative) = xml[cursor..].find('<') {
        let start = cursor + relative;
        let Some(end_rel) = xml[start..].find('>') else {
            break;
        };
        let end = start + end_rel;
        let tag = xml[start + 1..end].trim();
        let name = tag
            .strip_suffix('/')
            .map(str::trim)
            .and_then(|tag| tag.split_whitespace().next())
            .unwrap_or_default();
        if name.rsplit(':').next() == Some(local_name) {
            return true;
        }
        cursor = end + 1;
    }
    false
}

fn xml_blocks<'a>(xml: &'a str, local_name: &str) -> Vec<&'a str> {
    let mut result = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = xml[cursor..].find('<') {
        let start = cursor + relative;
        let Some(open_end_rel) = xml[start..].find('>') else {
            break;
        };
        let open_end = start + open_end_rel;
        let tag = xml[start + 1..open_end].trim();
        if tag.starts_with('/') || tag.starts_with('!') || tag.starts_with('?') {
            cursor = open_end + 1;
            continue;
        }
        let tag_name = tag
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .trim_end_matches('/');
        if tag_name.rsplit(':').next() != Some(local_name) || tag.ends_with('/') {
            cursor = open_end + 1;
            continue;
        }
        let content_start = open_end + 1;
        let mut scan = content_start;
        let found = loop {
            let Some(next_rel) = xml[scan..].find('<') else {
                break None;
            };
            let next = scan + next_rel;
            let Some(next_end_rel) = xml[next..].find('>') else {
                break None;
            };
            let next_end = next + next_end_rel;
            let next_tag = xml[next + 1..next_end].trim();
            if let Some(closing_name) = next_tag.strip_prefix('/') {
                if closing_name.trim().rsplit(':').next() == Some(local_name) {
                    break Some((next, next_end + 1));
                }
            }
            scan = next_end + 1;
        };
        let Some(end) = found else { break };
        result.push(&xml[content_start..end.0]);
        cursor = end.1;
    }
    result
}

fn xml_texts(xml: &str, local_name: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = xml[cursor..].find('<') {
        let start = cursor + relative;
        let Some(end_rel) = xml[start..].find('>') else {
            break;
        };
        let end = start + end_rel;
        let tag = xml[start + 1..end].trim();
        if tag.starts_with('/') || tag.starts_with('!') || tag.starts_with('?') {
            cursor = end + 1;
            continue;
        }
        let name = tag
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .trim_end_matches('/');
        if name.rsplit(':').next() != Some(local_name) || tag.ends_with('/') {
            cursor = end + 1;
            continue;
        }
        let close = format!("</{name}>");
        if let Some(close_rel) = xml[end + 1..].find(&close) {
            let content = &xml[end + 1..end + 1 + close_rel];
            values.push(strip_xml_text(content));
            cursor = end + 1 + close_rel + close.len();
        } else {
            cursor = end + 1;
        }
    }
    values
}

fn strip_xml_text(value: &str) -> String {
    let mut text = String::with_capacity(value.len());
    let mut in_tag = false;
    for ch in value.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(ch),
            _ => {}
        }
    }
    text.trim().replace("&amp;", "&").replace("&quot;", "\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiple_events_from_one_calendar_data_block() {
        let ics = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nDTSTART:20260925T090000Z\nDTEND:20260925T100000Z\nSUMMARY:Первое\nEND:VEVENT\nBEGIN:VEVENT\nDTSTART:20260925T110000Z\nDTEND:20260925T120000Z\nSUMMARY:Второе\nEND:VEVENT\nEND:VCALENDAR";
        let events = parse_digest_events(ics);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].title, "Первое");
        assert_eq!(events[1].title, "Второе");
    }

    #[test]
    fn treats_floating_ical_times_as_moscow_local_time() {
        assert_eq!(
            parse_ical_time("20260925T080000", false).unwrap(),
            "2026-09-25T08:00:00+03:00"
        );
        assert_eq!(
            parse_ical_time("20260925T050000Z", false).unwrap(),
            "2026-09-25T05:00:00+00:00"
        );
    }

    #[test]
    fn validates_calendar_event_times_and_ical_escaping() {
        let request = CalendarEventRequest {
            title: "Встреча, важная".into(),
            start_at: "2026-09-23T15:00:00+03:00".into(),
            end_at: "2026-09-23T16:00:00+03:00".into(),
            description: Some("Строка; с\nпереносом".into()),
            location: None,
        };
        let (start, end) = validate_event(&request).unwrap();
        let ics = build_icalendar("event-1", &request, start, end);
        assert!(ics.contains("SUMMARY:Встреча\\, важная"));
        assert!(ics.contains("DESCRIPTION:Строка\\; с\\nпереносом"));
        assert!(ics.contains("\r\nEND:VEVENT\r\n"));
    }

    #[test]
    fn parses_dotenv_values_without_exposing_secrets() {
        assert_eq!(parse_env_value("\"secret value\""), "secret value");
        assert_eq!(parse_env_value("plain # comment"), "plain");
        assert_eq!(parse_env_value("'quoted'"), "quoted");
    }

    #[test]
    fn rejects_missing_offset_and_reversed_range_before_network() {
        let missing_offset = CalendarEventRequest {
            title: "Встреча".into(),
            start_at: "2026-09-23T15:00:00".into(),
            end_at: "2026-09-23T16:00:00+03:00".into(),
            description: None,
            location: None,
        };
        assert!(validate_event(&missing_offset).is_err());
        let reversed = CalendarEventRequest {
            start_at: "2026-09-23T16:00:00+03:00".into(),
            end_at: "2026-09-23T15:00:00+03:00".into(),
            ..missing_offset
        };
        assert!(validate_event(&reversed).is_err());
    }

    #[test]
    fn parses_namespaced_calendar_collections() {
        let home = Url::parse("https://caldav.test/calendars/user/").unwrap();
        let xml = r#"<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><D:response><D:href>/calendars/user/main/</D:href><D:propstat><D:prop><D:displayname>Main</D:displayname><D:resourcetype><D:collection/><C:calendar/></D:resourcetype></D:prop></D:propstat></D:response></D:multistatus>"#;
        let collections = parse_calendar_collections(&home, xml).unwrap();
        assert_eq!(collections[0].display_name, "Main");
        assert_eq!(
            collections[0].href.as_str(),
            "https://caldav.test/calendars/user/main/"
        );
    }

    #[test]
    fn property_discovery_uses_nested_href_not_response_href() {
        let xml = r#"<D:multistatus xmlns:D="DAV:"><D:response><D:href>/</D:href><D:propstat><D:prop><D:current-user-principal><D:href>/principals/users/test/</D:href></D:current-user-principal></D:prop></D:propstat></D:response></D:multistatus>"#;
        assert_eq!(
            xml_texts(xml, "href").into_iter().last().as_deref(),
            Some("/principals/users/test/")
        );
    }

    #[test]
    fn calendar_selection_never_guesses_between_multiple_collections() {
        let collections = vec![
            CalendarCollection {
                href: Url::parse("https://caldav.test/main/").unwrap(),
                display_name: "Main".into(),
            },
            CalendarCollection {
                href: Url::parse("https://caldav.test/work/").unwrap(),
                display_name: "Work".into(),
            },
        ];
        assert!(choose_calendar(&collections, None).is_err());
        assert_eq!(
            choose_calendar(&collections, Some("Work"))
                .unwrap()
                .display_name,
            "Work"
        );
        assert!(choose_calendar(&collections, Some("Missing")).is_err());
    }
}
