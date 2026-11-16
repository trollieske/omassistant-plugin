//! The web, in the background: one search, one page, nothing else.
//!
//! An assistant that has to screenshot a page to read a football result is both
//! expensive and slow, and it looks limited. This module is the alternative:
//! `web.search` asks a news index that is meant for machines (RSS, not a scraped
//! HTML page), and `web.fetch` reads one public page as text.
//!
//! # Why RSS
//!
//! Every keyless HTML search endpoint answers a bot with a captcha (DuckDuckGo
//! serves a challenge, Mojeek a captcha page from this machine). Feeds are the
//! endpoint those services publish *for* clients, so they answer a plain `curl`
//! with titles, links and dates. No key, no account, nothing to sign up for.
//!
//! # What is refused
//!
//! Only `http` and `https`, and only public hosts: a fetch to `127.0.0.1`, an RFC
//! 1918 address, link-local, `.local` or a bare `localhost` would let the model
//! poke at the person's own services or the machine's metadata endpoint. The
//! address is checked after resolution, so a public name that points at a private
//! address is refused too.
//!
//! The output is text with the tags gone, capped. Nothing here stores a page: the
//! body goes to the caller and the audit keeps the host and the length.

use crate::error::{ApiError, ErrorCode};
use crate::exec::one_line;
use std::net::IpAddr;

/// The longest search query this daemon will send.
pub const MAX_QUERY_CHARS: usize = 200;
/// The longest URL this daemon will fetch.
pub const MAX_URL_CHARS: usize = 2000;
/// The most text returned from one page.
pub const MAX_TEXT_CHARS: usize = 20_000;
/// The most bytes read from one page. A Wikipedia article is over a megabyte
/// with its markup, and the text we keep from it is a few kilobytes; the cap is
/// here to stop a hostile page, not a real one.
pub const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
/// How many search hits are returned.
pub const MAX_HITS: usize = 8;
/// How much of a result is carried with it. A Wikipedia intro puts the number a
/// person asked for a sentence or two in, so 240 characters cut off the answer;
/// 400 fits the whole opening paragraph of a city article.
pub const SNIPPET_CHARS: usize = 400;
/// The user agent the search index sees. Honest, and a browser UA buys nothing
/// from a feed.
pub const USER_AGENT: &str = "omassistant/0.1 (+https://github.com/trollieske/omassistant-plugin)";
/// What to say when the person asks for the web.
pub const SEARCH_ENDPOINT: &str = "https://news.google.com/rss/search";

/// Which search service answers. `Keyless` is the built-in news + encyclopedia
/// path: no account, no cost, and a real ceiling on what it can find. The others
/// are a real web index, which is what "ask it anything" needs, and they need a
/// key the person supplies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// No key: a news feed and an encyclopedia.
    Keyless,
    /// Brave Search API: `X-Subscription-Token`.
    Brave,
    /// Tavily: an index built for agents, returns page text with each result.
    Tavily,
    /// Serper: Google results as JSON.
    Serper,
    /// You.com: an index built for agents, with page text and thumbnails.
    You,
}

impl Provider {
    /// The name written in the config file.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Keyless => "keyless",
            Self::Brave => "brave",
            Self::Tavily => "tavily",
            Self::Serper => "serper",
            Self::You => "you",
        }
    }

    /// Read a provider by name, for the config file and the settings panel.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "keyless" | "" => Some(Self::Keyless),
            "brave" => Some(Self::Brave),
            "tavily" => Some(Self::Tavily),
            "serper" => Some(Self::Serper),
            "you" | "you.com" => Some(Self::You),
            _ => None,
        }
    }

    /// The hosts this provider talks to, for the permission row.
    #[must_use]
    pub fn host(self) -> &'static str {
        match self {
            Self::Keyless => "news.google.com, wikipedia.org",
            Self::Brave => "api.search.brave.com",
            Self::Tavily => "api.tavily.com",
            Self::Serper => "google.serper.dev",
            Self::You => "api.you.com",
        }
    }
}

/// The search key the person configured. It lives in the daemon's state
/// directory, mode 0600, and never leaves the machine except as the header the
/// provider itself expects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Who answers.
    pub provider: Provider,
    /// The API key, empty for `Keyless`.
    pub key: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: Provider::Keyless,
            key: String::new(),
        }
    }
}

/// The config file inside the state directory.
pub const CONFIG_FILE: &str = "web.json";

/// Read the provider config. A missing file is the keyless default; a file that
/// names a provider we do not know is ignored rather than trusted.
#[must_use]
pub fn load_config(state_dir: &std::path::Path) -> Config {
    let path = state_dir.join(CONFIG_FILE);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Config::default();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Config::default();
    };
    let provider = value["provider"]
        .as_str()
        .and_then(Provider::parse)
        .unwrap_or(Provider::Keyless);
    let key = value["key"].as_str().unwrap_or("").trim().to_string();
    if provider != Provider::Keyless && key.is_empty() {
        // A provider without a key would fail every call; say so by falling back.
        return Config::default();
    }
    Config { provider, key }
}

/// Write the provider config with mode 0600: a key in a world-readable file is a
/// key someone else has.
pub fn write_config(state_dir: &std::path::Path, config: &Config) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    std::fs::create_dir_all(state_dir)?;
    let path = state_dir.join(CONFIG_FILE);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?;
    let body = serde_json::json!({
        "provider": config.provider.name(),
        "key": config.key,
    });
    file.write_all(body.to_string().as_bytes())?;
    file.flush()?;
    // `mode` only applies when the file is created; an existing file keeps its
    // bits, so set them every time.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// One HTTP request to run: a URL, an optional authorization header, and a body
/// for the providers that want POST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// Where to send it.
    pub url: String,
    /// `Name: value`, added with `-H`.
    pub header: Option<String>,
    /// JSON body, sent with `--data-binary @-`.
    pub body: Option<String>,
}

/// Build the request for one provider and one query.
#[must_use]
pub fn build(provider: Provider, key: &str, query: &str) -> Request {
    match provider {
        Provider::Brave => Request {
            url: format!(
                "https://api.search.brave.com/res/v1/web/search?count=8&q={}",
                percent_encode(query)
            ),
            header: Some(format!("X-Subscription-Token: {key}")),
            body: None,
        },
        Provider::Tavily => Request {
            url: "https://api.tavily.com/search".to_string(),
            header: None,
            body: Some(
                serde_json::json!({
                    "api_key": key,
                    "query": query,
                    "max_results": 6,
                    "include_answer": false,
                    "search_depth": "basic",
                })
                .to_string(),
            ),
        },
        Provider::Serper => Request {
            url: "https://google.serper.dev/search".to_string(),
            header: Some(format!("X-API-KEY: {key}")),
            body: Some(
                serde_json::json!({
                    "q": query,
                    "num": 8,
                })
                .to_string(),
            ),
        },
        Provider::You => Request {
            url: format!(
                "https://api.you.com/v1/search?query={}&count=8",
                percent_encode(query)
            ),
            header: Some(format!("X-API-Key: {key}")),
            body: None,
        },
        Provider::Keyless => Request {
            url: search_url(query),
            header: None,
            body: None,
        },
    }
}

/// Parse a provider's answer into hits.
pub fn parse_results(provider: Provider, body: &str) -> Result<Vec<Hit>, ApiError> {
    match provider {
        Provider::Keyless => parse_feed(body),
        Provider::Brave => parse_brave(body),
        Provider::Tavily => parse_tavily(body),
        Provider::Serper => parse_serper(body),
        Provider::You => parse_you(body),
    }
}

/// Brave: `web.results[]` with `title`, `url`, `description`, `age`.
fn parse_brave(body: &str) -> Result<Vec<Hit>, ApiError> {
    let value = parse_json(body)?;
    let mut hits = Vec::new();
    if let Some(results) = value["web"]["results"].as_array() {
        for item in results {
            let (Some(title), Some(url)) = (item["title"].as_str(), item["url"].as_str()) else {
                continue;
            };
            hits.push(Hit {
                title: one_line(&html_to_text(title), 200),
                url: url.to_string(),
                when: one_line(item["age"].as_str().unwrap_or(""), 64),
                source: "web",
                image: String::new(),
                snippet: one_line(
                    &html_to_text(item["description"].as_str().unwrap_or("")),
                    SNIPPET_CHARS,
                ),
            });
        }
    }
    Ok(hits)
}

/// Tavily: `results[]` with `title`, `url`, `content`.
fn parse_tavily(body: &str) -> Result<Vec<Hit>, ApiError> {
    let value = parse_json(body)?;
    let mut hits = Vec::new();
    for item in value["results"].as_array().into_iter().flatten() {
        let (Some(title), Some(url)) = (item["title"].as_str(), item["url"].as_str()) else {
            continue;
        };
        hits.push(Hit {
            title: one_line(title, 200),
            url: url.to_string(),
            when: String::new(),
            source: "web",
            image: String::new(),
            snippet: one_line(item["content"].as_str().unwrap_or(""), SNIPPET_CHARS),
        });
    }
    Ok(hits)
}

/// Serper: `organic[]` with `title`, `link`, `snippet`, `date`.
fn parse_serper(body: &str) -> Result<Vec<Hit>, ApiError> {
    let value = parse_json(body)?;
    let mut hits = Vec::new();
    for item in value["organic"].as_array().into_iter().flatten() {
        let (Some(title), Some(url)) = (item["title"].as_str(), item["link"].as_str()) else {
            continue;
        };
        hits.push(Hit {
            title: one_line(&html_to_text(title), 200),
            url: url.to_string(),
            when: one_line(item["date"].as_str().unwrap_or(""), 64),
            source: "web",
            image: String::new(),
            snippet: one_line(item["snippet"].as_str().unwrap_or(""), SNIPPET_CHARS),
        });
    }
    Ok(hits)
}

/// You.com: `results.web[]` with `title`, `url`, `description`, `snippets`,
/// `page_age` and a thumbnail. The description is page text, not a headline, which
/// is why one call is usually enough to answer.
fn parse_you(body: &str) -> Result<Vec<Hit>, ApiError> {
    let value = parse_json(body)?;
    let mut hits = Vec::new();
    for item in value["results"]["web"].as_array().into_iter().flatten() {
        let (Some(title), Some(url)) = (item["title"].as_str(), item["url"].as_str()) else {
            continue;
        };
        let description = item["description"].as_str().unwrap_or("");
        let snippet = if description.trim().is_empty() {
            let first = item["snippets"]
                .as_array()
                .and_then(|list| list.first())
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            one_line(&html_to_text(first), SNIPPET_CHARS)
        } else {
            one_line(&html_to_text(description), SNIPPET_CHARS)
        };
        hits.push(Hit {
            title: one_line(&html_to_text(title), 200),
            url: url.to_string(),
            when: one_line(item["page_age"].as_str().unwrap_or(""), 64),
            source: "web",
            image: one_line(
                item["original_thumbnail_url"]
                    .as_str()
                    .or_else(|| item["thumbnail_url"].as_str())
                    .unwrap_or(""),
                500,
            ),
            snippet,
        });
    }
    Ok(hits)
}

fn parse_json(body: &str) -> Result<serde_json::Value, ApiError> {
    serde_json::from_str(body).map_err(|_| {
        ApiError::new(
            ErrorCode::Unavailable,
            "the search service did not answer with JSON",
        )
    })
}

/// One search result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// The headline.
    pub title: String,
    /// The link.
    pub url: String,
    /// When it was published, as the index spells it (empty for an article).
    pub when: String,
    /// Where it came from: `news`, `wikipedia`, `web`.
    pub source: &'static str,
    /// A picture the result came with, when the index offers one. Empty most of
    /// the time; the club badge is the reason the field exists.
    pub image: String,
    /// A sentence or two from the result, so the model can answer without opening
    /// the page. This is what makes one lookup enough.
    pub snippet: String,
}

/// Check a search query and return it unchanged.
pub fn validate_query(raw: &str) -> Result<String, ApiError> {
    let query = raw.trim();
    if query.is_empty() {
        return Err(ApiError::invalid("a search query is required"));
    }
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(ApiError::invalid(format!(
            "a search query may not be longer than {MAX_QUERY_CHARS} characters"
        )));
    }
    if query.chars().any(|ch| ch.is_control()) {
        return Err(ApiError::invalid("a search query must be one line"));
    }
    Ok(query.to_string())
}

/// The URL to ask the feed for, with the person's language when we can tell it.
#[must_use]
pub fn search_url(query: &str) -> String {
    let (hl, gl, ceid) = language_params();
    format!(
        "{SEARCH_ENDPOINT}?q={}&hl={hl}&gl={gl}&ceid={ceid}",
        percent_encode(query)
    )
}

/// Language for the feed, from the session's locale. Norwegian gets Norwegian
/// sources; everything else falls back to English, which every feed carries.
fn language_params() -> (&'static str, &'static str, &'static str) {
    let locale = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default()
        .to_ascii_lowercase();
    if locale.starts_with("nb") || locale.starts_with("nn") || locale.starts_with("no") {
        ("nb", "NO", "NO:nb")
    } else {
        ("en-US", "US", "US:en")
    }
}

/// The encyclopedia, for the questions a news index cannot answer.
pub const WIKI_ENDPOINT: &str = "https://LANG.wikipedia.org/w/api.php";

/// Which Wikipedia to read: the person's language when it has one, English
/// otherwise.
#[must_use]
pub fn wiki_language() -> &'static str {
    let locale = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default()
        .to_ascii_lowercase();
    if locale.starts_with("nb") || locale.starts_with("nn") || locale.starts_with("no") {
        "no"
    } else {
        "en"
    }
}

/// Words that ask a question rather than name a thing, in both languages the
/// person writes in. A search field would drop them; an index does not, and
/// "hvor mange innbyggere har Bergen" finds nothing in an English encyclopedia
/// while "Bergen" finds the city.
pub const STOPWORDS: &[&str] = &[
    // Norwegian
    "hva", "hvor", "hvem", "hvilken", "hvilket", "hvilke", "når", "hvorfor", "hvordan", "er", "var",
    "har", "hadde", "kan", "kunne", "skal", "skulle", "vil", "må", "mange", "mye", "noen", "noe",
    "det", "den", "de", "et", "en", "og", "i", "på", "for", "til", "av", "med", "som", "om", "jeg",
    "du", "vi", "meg", "deg", "seg", "sin", "sitt", "meg", "nå", "akkurat", "egentlig", "finn",
    "finnes", "si", "si meg", "fortell", // English
    "what", "who", "whom", "whose", "when", "where", "why", "how", "which", "is", "are", "was",
    "were", "am", "the", "a", "an", "of", "in", "on", "at", "to", "for", "with", "and", "or", "do",
    "does", "did", "can", "could", "should", "would", "will", "many", "much", "some", "any",
    "there", "it", "this", "that", "these", "those", "i", "you", "we", "me", "my", "your", "tell",
    "about", "please", "give",
];

/// The same question with the asking words taken out, or `None` when that changes
/// nothing. This is the query a search engine would run.
#[must_use]
pub fn keywords(query: &str) -> Option<String> {
    let cleaned: Vec<&str> = query
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|ch: char| ch.is_ascii_punctuation() || ch == '«' || ch == '»')
        })
        .filter(|word| !word.is_empty())
        .filter(|word| {
            let lower = word.to_lowercase();
            !STOPWORDS.contains(&lower.as_str())
        })
        .collect();
    if cleaned.is_empty() {
        return None;
    }
    let joined = cleaned.join(" ");
    if joined.eq_ignore_ascii_case(query.trim()) {
        return None;
    }
    Some(joined)
}

/// The URL that searches the encyclopedia for `query`.
///
/// `generator=search` with `prop=extracts` returns the articles *and* their
/// opening text in one answer. Without the extract the model gets a title, has to
/// fetch the page, and the whole lookup costs two round trips for a fact the
/// index could have handed over at once.
#[must_use]
pub fn wiki_url(query: &str) -> String {
    wiki_url_in(wiki_language(), query)
}

/// The same search in a named language.
#[must_use]
pub fn wiki_url_in(language: &str, query: &str) -> String {
    format!(
        "{}?action=query&generator=search&gsrsearch={}&gsrlimit=4&prop=extracts|info&exintro=1&explaintext=1&inprop=url&format=json&utf8=1",
        WIKI_ENDPOINT.replace("LANG", language),
        percent_encode(query)
    )
}

/// The article URL for a title, with the wiki's own spelling of spaces.
#[must_use]
pub fn wiki_article_url(title: &str) -> String {
    wiki_article_url_in(wiki_language(), title)
}

/// The article URL in a named language.
#[must_use]
pub fn wiki_article_url_in(language: &str, title: &str) -> String {
    let slug = title.trim().replace(' ', "_");
    format!(
        "https://{language}.wikipedia.org/wiki/{}",
        percent_encode_path(&slug)
    )
}

/// Percent-encode a path segment: spaces became underscores already, and `&`,
/// `?` and `#` must not start a query string.
fn percent_encode_path(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            'A'..='Z'
            | 'a'..='z'
            | '0'..='9'
            | '-'
            | '_'
            | '.'
            | '~'
            | '('
            | ')'
            | ','
            | '\''
            | '/' => out.push(ch),
            ' ' => out.push('_'),
            other => {
                let mut buffer = [0u8; 4];
                for byte in other.encode_utf8(&mut buffer).as_bytes() {
                    out.push_str(&format!("%{byte:02X}"));
                }
            }
        }
    }
    out
}

/// Parse the encyclopedia's JSON answer into hits. A malformed answer is an
/// error, an empty one is an empty list.
pub fn parse_wiki(body: &str) -> Result<Vec<Hit>, ApiError> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|_| {
        ApiError::new(
            ErrorCode::Unavailable,
            "the encyclopedia did not answer with JSON",
        )
    })?;
    let Some(pages) = value["query"]["pages"].as_object() else {
        return Ok(Vec::new());
    };
    // Pages arrive keyed by id, in whatever order the map keeps them; the search
    // order is in `index`, when the generator supplies it.
    let mut pages: Vec<&serde_json::Value> = pages.values().collect();
    pages.sort_by_key(|page| page["index"].as_u64().unwrap_or(u64::MAX));
    Ok(pages
        .into_iter()
        .filter_map(|page| {
            let title = page["title"].as_str()?.trim();
            if title.is_empty() {
                return None;
            }
            let url = page["fullurl"]
                .as_str()
                .map_or_else(|| wiki_article_url(title), str::to_string);
            let extract = page["extract"].as_str().unwrap_or("");
            // The best article gets its whole opening paragraph: the number a
            // person asked for is often in the third sentence, past the cap.
            let cap = if page["index"].as_u64() == Some(1) || page["index"].is_null() {
                SNIPPET_CHARS * 3
            } else {
                SNIPPET_CHARS
            };
            Some(Hit {
                title: one_line(title, 200),
                url,
                when: String::new(),
                source: "wikipedia",
                image: String::new(),
                snippet: one_line(extract, cap),
            })
        })
        .take(4)
        .collect())
}

/// Parse the feed into hits. A feed that answers with something else is an error,
/// not an empty result: "no news" and "not a feed" are different facts.
pub fn parse_feed(body: &str) -> Result<Vec<Hit>, ApiError> {
    let mut hits = Vec::new();
    for block in body.split("<item>").skip(1) {
        let item = block.split("</item>").next().unwrap_or_default();
        let title = html_to_text(field(item, "title"));
        let url = field(item, "link").trim().to_string();
        let snippet = one_line(&html_to_text(field(item, "description")), SNIPPET_CHARS);
        if title.is_empty() || url.is_empty() {
            continue;
        }
        hits.push(Hit {
            title: one_line(&title, 200),
            url: one_line(&url, MAX_URL_CHARS),
            when: one_line(field(item, "pubDate"), 64),
            source: "news",
            image: String::new(),
            snippet,
        });
        if hits.len() >= MAX_HITS {
            break;
        }
    }
    if hits.is_empty() {
        // A feed with nothing in it is still a feed; a challenge page is not.
        if body.contains("<rss") || body.contains("<feed") {
            return Ok(hits);
        }
        return Err(ApiError::new(
            ErrorCode::Unavailable,
            "the search index did not answer with a feed",
        ));
    }
    Ok(hits)
}

/// The text between `<tag>` and `</tag>`, if both are there.
fn field<'a>(item: &'a str, tag: &str) -> &'a str {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let Some(start) = item.find(&open) else {
        return "";
    };
    let rest = &item[start + open.len()..];
    let Some(end) = rest.find(&close) else {
        return "";
    };
    &rest[..end]
}

/// Check a URL before fetching it: `http(s)` only, public host only.
pub fn validate_url(raw: &str) -> Result<String, ApiError> {
    let url = raw.trim();
    if url.is_empty() {
        return Err(ApiError::invalid("a URL is required"));
    }
    if url.chars().count() > MAX_URL_CHARS {
        return Err(ApiError::invalid(format!(
            "a URL may not be longer than {MAX_URL_CHARS} characters"
        )));
    }
    if url.chars().any(|ch| ch.is_control() || ch.is_whitespace()) {
        return Err(ApiError::invalid("a URL must be one word, without spaces"));
    }
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| ApiError::invalid("\"url\" must start with http:// or https://"))?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(ApiError::invalid("only http and https URLs can be fetched"));
    }
    // Checked on the authority, not the host: `host_of` already split the
    // credentials off, so a `user:pass@example.com` would look like host `user`.
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.contains('@') {
        return Err(ApiError::invalid(
            "a URL with a username or password is not fetched",
        ));
    }
    let host = host_of(rest).ok_or_else(|| ApiError::invalid("the URL has no host"))?;
    if !host_is_public(&host) {
        return Err(ApiError::invalid(
            "that host is this machine or a private network, so it is not fetched",
        ));
    }
    Ok(url.to_string())
}

/// The host part of `host[:port]/path`, without the port.
fn host_of(rest: &str) -> Option<String> {
    let authority = rest.split('/').next().unwrap_or_default();
    let authority = authority.split(['?', '#']).next().unwrap_or_default();
    if authority.is_empty() {
        return None;
    }
    let host = if let Some(stripped) = authority.strip_prefix('[') {
        // An IPv6 literal: keep what is inside the brackets.
        stripped.split(']').next().map(str::to_string)?
    } else {
        authority.split(':').next().unwrap_or_default().to_string()
    };
    Some(host.to_ascii_lowercase())
}

/// Is this a host on the public internet?
fn host_is_public(host: &str) -> bool {
    if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
        return false;
    }
    if host.ends_with(".localhost") || host.ends_with(".local") || host.ends_with(".internal") {
        return false;
    }
    // A bare IP is checked directly; a name is checked after resolution so a
    // public name pointing at a private address is refused too.
    if let Ok(address) = host.parse::<IpAddr>() {
        return is_public_ip(address);
    }
    match std::net::ToSocketAddrs::to_socket_addrs(&(host, 443)) {
        Ok(addresses) => addresses.map(|addr| addr.ip()).all(is_public_ip),
        // A name that does not resolve is not a private host; curl will say so.
        Err(_) => true,
    }
}

/// Is this address routable on the public internet?
fn is_public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => {
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast())
        }
        IpAddr::V6(v6) => {
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // Unique-local (fc00::/7) and link-local (fe80::/10).
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                // An IPv4 address mapped into IPv6 is only as public as the IPv4.
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| !is_public_ip(IpAddr::V4(v4))))
        }
    }
}

/// Turn a page into something a model can read: no scripts, no styles, no tags.
///
/// Entities are decoded *first*: a feed writes its description as `&lt;p&gt;` when
/// the CMS stored HTML, and stripping tags before decoding leaves that markup
/// visible as text — which is how a "snippet" arrives full of `<a href=`.
#[must_use]
pub fn html_to_text(html: &str) -> String {
    let decoded = decode_entities(html);
    let mut cleaned = strip_comments(&decoded);
    // Page chrome is noise: a navigation menu read aloud buries the paragraph.
    for tag in [
        "script", "style", "noscript", "svg", "nav", "header", "footer", "aside", "form", "iframe",
        "template",
    ] {
        cleaned = strip_blocks(&cleaned, tag);
    }
    // The body, when the page has one: headers and navigation are noise.
    let rest: &str = match cleaned.find("<body") {
        Some(start) => &cleaned[start..],
        None => &cleaned,
    };
    let mut out = String::new();
    let mut chars = rest.chars().peekable();
    let mut in_tag = false;
    while let Some(ch) = chars.next() {
        if in_tag {
            if ch == '>' {
                in_tag = false;
            }
            continue;
        }
        if ch == '<' {
            // A tag starts with a name, a closing slash or a comment; a `5 < 6`
            // in prose does not.
            let next = chars.peek().copied().unwrap_or(' ');
            if next.is_ascii_alphabetic() || next == '/' || next == '!' {
                in_tag = true;
                // A tag is a word boundary; keep the words apart.
                if !out.ends_with(' ') {
                    out.push(' ');
                }
                continue;
            }
        }
        out.push(ch);
        if out.chars().count() >= MAX_TEXT_CHARS * 2 {
            break;
        }
    }
    // A feed that escaped its HTML twice leaves `&nbsp;` behind after one pass.
    collapse(&decode_entities(&out))
}

/// Remove `<!-- … -->`, which is where a page keeps code and configuration that
/// is not prose.
fn strip_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(start) = input[cursor..].find("<!--") {
        let start = cursor + start;
        out.push_str(&input[cursor..start]);
        match input[start..].find("-->") {
            Some(end) => cursor = start + end + 3,
            None => {
                cursor = input.len();
                break;
            }
        }
    }
    if cursor < input.len() {
        out.push_str(&input[cursor..]);
    }
    out
}

/// Remove every `<tag>…</tag>` block, case-insensitively.
fn strip_blocks(input: &str, tag: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(start) = lower[cursor..].find(&open) {
        let start = cursor + start;
        out.push_str(&input[cursor..start]);
        match lower[start..].find(&close) {
            Some(end) => cursor = start + end + close.len(),
            None => {
                cursor = input.len();
                break;
            }
        }
    }
    if cursor < input.len() {
        out.push_str(&input[cursor..]);
    }
    out
}

/// Decode the entities a feed or page uses in prose.
fn decode_entities(text: &str) -> String {
    let mut out = text
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'");
    // Numeric entities, decimal and hex.
    for (marker, radix) in [("&#x", 16u32), ("&#", 10u32)] {
        while let Some(start) = out.find(marker) {
            let rest = &out[start + marker.len()..];
            let Some(end) = rest.find(';') else { break };
            let digits = &rest[..end];
            let Ok(value) = u32::from_str_radix(digits, radix) else {
                break;
            };
            let Some(ch) = char::from_u32(value) else {
                break;
            };
            out.replace_range(start..start + marker.len() + end + 1, &ch.to_string());
        }
    }
    out
}

/// Keep the text one readable block: no runs of spaces, no blank lines.
fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(ch);
        if out.chars().count() >= MAX_TEXT_CHARS {
            break;
        }
    }
    out
}

/// Percent-encode everything that is not unreserved, for a query string.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len() * 3);
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn a_query_is_one_line_and_capped() {
        assert_eq!(
            validate_query("  football results  ").unwrap(),
            "football results"
        );
        assert!(validate_query("").is_err());
        assert!(validate_query("two\nlines").is_err());
        assert!(validate_query(&"x".repeat(MAX_QUERY_CHARS + 1)).is_err());
        assert!(validate_query(&"x".repeat(MAX_QUERY_CHARS)).is_ok());
    }

    #[test]
    fn the_search_url_carries_the_query_encoded() {
        let url = search_url("Vålerenga resultat & mål");
        assert!(url.starts_with(SEARCH_ENDPOINT));
        assert!(
            url.contains("q=V%C3%A5lerenga+resultat+%26+m%C3%A5l"),
            "{url}"
        );
        assert!(
            !url.contains(' '),
            "a space would end the URL at the shell: {url}"
        );
    }

    #[test]
    fn only_public_http_urls_are_fetched() {
        for good in [
            "https://en.wikipedia.org/wiki/Football",
            "http://example.com",
            "https://www.tv2.no/sport/",
        ] {
            assert!(validate_url(good).is_ok(), "{good}");
        }
        for bad in [
            "http://localhost:11434/api/tags",
            "http://127.0.0.1/",
            "http://[::1]/",
            "http://192.168.1.1/",
            "http://10.0.0.5/",
            "http://172.16.0.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://printer.local/",
            "file:///etc/passwd",
            "gopher://example.com",
            "https://user:pass@example.com/",
            "https://example.com/a b",
            "",
        ] {
            assert!(validate_url(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn a_you_answer_becomes_hits_with_its_picture() {
        let body = r#"{"results":{"web":[
            {"title":"Vålerenga - kamper","url":"https://example.com/v",
             "description":"Get the latest Vålerenga results and fixtures.",
             "original_thumbnail_url":"https://images.example.com/badge.png","page_age":null},
            {"title":"Sofascore","url":"https://example.com/s","description":"",
             "snippets":["Vålerenga live score and results"]}
        ]}}"#;
        let hits = parse_you(body).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].source, "web");
        assert_eq!(hits[0].title, "Vålerenga - kamper");
        assert_eq!(hits[0].image, "https://images.example.com/badge.png");
        assert_eq!(hits[0].when, "");
        assert!(hits[0].snippet.contains("latest Vålerenga results"));
        // No description: the snippet list is the text.
        assert_eq!(hits[1].snippet, "Vålerenga live score and results");
        assert_eq!(hits[1].image, "");
        // A captcha page is an error, not an empty result.
        assert!(parse_you("<html>no</html>").is_err());
    }

    #[test]
    fn a_feed_becomes_hits_and_a_challenge_is_an_error() {
        let feed = r#"<rss><channel>
            <item><title>United 2 &amp; City 1</title><link>https://example.com/a</link>
              <pubDate>Sun, 15 Nov 2026 08:30:36 GMT</pubDate>
              <description><![CDATA[<a href="x">ignored</a>]]></description></item>
            <item><title>Second</title><link>https://example.com/b</link></item>
            </channel></rss>"#;
        let hits = parse_feed(feed).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "United 2 & City 1");
        assert_eq!(hits[0].url, "https://example.com/a");
        assert!(hits[0].when.starts_with("Sun, 15 Nov"));

        // An empty feed is not a failure…
        assert_eq!(
            parse_feed("<rss><channel></channel></rss>").unwrap().len(),
            0
        );
        // …but a challenge page is.
        assert!(parse_feed("<html>are you a robot</html>").is_err());
    }

    #[test]
    fn an_encyclopedia_answer_becomes_hits() {
        let body = r#"{"query":{"pages":{
            "2":{"index":2,"title":"Eliteserien","fullurl":"https://en.wikipedia.org/wiki/Eliteserien","extract":"the top league"},
            "1":{"index":1,"title":"Vålerenga Fotball","fullurl":"https://en.wikipedia.org/wiki/V%C3%A5lerenga_Fotball","extract":"a club in Oslo"}
        }}}"#;
        let hits = parse_wiki(body).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0].title, "Vålerenga Fotball",
            "search order, not map order"
        );
        assert_eq!(hits[0].source, "wikipedia");
        assert_eq!(hits[0].snippet, "a club in Oslo");
        assert!(
            hits[0].url.ends_with("/wiki/V%C3%A5lerenga_Fotball"),
            "{}",
            hits[0].url
        );
        assert!(hits[0].when.is_empty());
        // A page with no results is not an error.
        assert!(parse_wiki(r#"{"query":{"pages":{}}}"#).unwrap().is_empty());
        // A challenge page is.
        assert!(parse_wiki("<html>robot</html>").is_err());
    }

    #[test]
    fn a_wiki_title_is_a_url_segment() {
        assert!(wiki_article_url("Bodø/Glimt").ends_with("/wiki/Bod%C3%B8/Glimt"));
        assert!(wiki_article_url("  Eliteserien  ").ends_with("/wiki/Eliteserien"));
    }

    #[test]
    fn an_escaped_snippet_loses_its_markup() {
        // What a feed really carries: the CMS stored HTML, so the description is
        // escaped once more.
        let feed = r#"<rss><channel><item>
            <title>Bergen</title><link>https://example.com/b</link>
            <description>&lt;a href="https://example.com"&gt;Bergen&lt;/a&gt; har 291 000 innbyggere.</description>
        </item></channel></rss>"#;
        let hits = parse_feed(feed).unwrap();
        assert_eq!(hits[0].snippet, "Bergen har 291 000 innbyggere.");
        // And prose that only looks like markup keeps its comparison.
        assert_eq!(
            html_to_text("<p>5 &lt; 6 &amp;&amp; 7 &gt; 2</p>"),
            "5 < 6 && 7 > 2"
        );
    }

    #[test]
    fn a_page_becomes_readable_text() {
        let html = r#"<html><head><style>p{color:red}</style>
            <script>var x = 1;</script></head>
            <body><h1>Title &amp; more</h1><p>Hello&nbsp;world &#8211; ok</p>
            <div>second   line</div></body></html>"#;
        let text = html_to_text(html);
        assert!(text.contains("Title & more"), "{text}");
        assert!(text.contains("Hello world – ok"), "{text}");
        assert!(text.contains("second line"), "{text}");
        assert!(!text.contains("color:red"), "{text}");
        assert!(!text.contains("var x"), "{text}");
        assert!(!text.contains("hidden"), "{text}");
        assert!(!text.contains('<'), "{text}");
    }
}
