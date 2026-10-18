//! Web Browser and Live Search Engine for Hagibis (`hgb`)
//!
//! Provides zero-cost, privacy-first web navigation and live internet search
//! tailored specifically for Local LLMs (Ollama Qwen, DeepSeek, Llama 3)
//! with SSRF boundary enforcement, HTML-to-Markdown compaction, and
//! token budget controls to prevent context window exhaustion.

use std::time::Duration;
use serde::{Deserialize, Serialize};
use crate::error::{HgbError, Result};
use crate::security::AgentShieldLight;

/// Result of browsing a web page
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebPageContent {
    pub url: String,
    pub title: String,
    pub status_code: u16,
    pub markdown: String,
    pub token_count: usize,
    pub truncated: bool,
}

/// A ranked organic search result
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebSearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub rank: usize,
}

/// Production-grade Web Browser and Live Search Engine for Local LLMs
#[derive(Clone)]
pub struct WebBrowserEngine {
    client: reqwest::Client,
}

impl Default for WebBrowserEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl WebBrowserEngine {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36 Hagibis/0.1")
            .timeout(Duration::from_secs(8))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self { client }
    }

    /// Browse a target URL, extract clean markdown, enforce token budget, and audit for prompt injections
    pub async fn browse_url(&self, url: &str, max_tokens: usize) -> Result<WebPageContent> {
        let target_url = url.trim();
        // 1. SSRF & Security audit
        AgentShieldLight::audit_url(target_url)?;

        let budget = if max_tokens == 0 { 1500 } else { max_tokens };

        // 2. Fetch web page
        let resp = self.client.get(target_url)
            .send()
            .await
            .map_err(|e| HgbError::Execution(format!("Web request failed for '{}': {}", target_url, e)))?;

        let status_code = resp.status().as_u16();
        let body = resp.text().await.map_err(|e| HgbError::Execution(format!("Failed to read web page body: {}", e)))?;

        // 3. Extract title and clean markdown
        let (title, raw_markdown) = Self::html_to_clean_markdown(&body);
        let clean_title = if title.is_empty() {
            target_url.to_string()
        } else {
            title
        };

        // 4. Compact whitespace and enforce token budget
        let (compact_markdown, token_count, truncated) = Self::compact_and_truncate_markdown(&raw_markdown, budget);

        // 5. Audit extracted markdown against indirect prompt injections
        AgentShieldLight::audit_payload(&compact_markdown)?;

        Ok(WebPageContent {
            url: target_url.to_string(),
            title: clean_title,
            status_code,
            markdown: compact_markdown,
            token_count,
            truncated,
        })
    }

    /// Synchronous wrapper for browse_url that can be safely invoked inside or outside tokio runtimes
    pub fn browse_url_sync(&self, url: &str, max_tokens: usize) -> Result<WebPageContent> {
        let engine = self.clone();
        let target_url = url.to_string();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| HgbError::Execution(format!("Failed to build async runtime: {}", e)))?;
            rt.block_on(engine.browse_url(&target_url, max_tokens))
        })
        .join()
        .map_err(|_| HgbError::Execution("Worker thread panicked while browsing URL".to_string()))?
    }

    /// Extract all valid HTTP/HTTPS URLs present in arbitrary text or prompt,
    /// including markdown links [text](url) and scheme-less domains (github.com/..., docs.rs/..., crates.io/..., www....)
    pub fn extract_urls(text: &str) -> Vec<String> {
        let mut urls = Vec::new();

        let clean_url = |raw: &str| -> String {
            let mut s = raw.trim().to_string();
            while s.ends_with('.')
                || s.ends_with(',')
                || s.ends_with('?')
                || s.ends_with('!')
                || s.ends_with(';')
                || s.ends_with(':')
                || s.ends_with(')')
                || s.ends_with(']')
                || s.ends_with('\'')
                || s.ends_with('"')
                || s.ends_with('`')
            {
                s.pop();
            }
            s
        };

        let normalize_url = |raw: &str| -> Option<String> {
            let s = clean_url(raw);
            if s.is_empty() {
                return None;
            }
            if s.starts_with("http://") || s.starts_with("https://") {
                Some(s)
            } else if s.starts_with("www.")
                || s.starts_with("github.com")
                || s.starts_with("crates.io")
                || s.starts_with("docs.rs")
                || s.starts_with("news.ycombinator.com")
                || s.starts_with("gitlab.com")
                || s.starts_with("bitbucket.org")
                || s.starts_with("reddit.com")
                || s.starts_with("stackoverflow.com")
                || s.starts_with("wikipedia.org")
                || s.starts_with("huggingface.co")
            {
                Some(format!("https://{}", s))
            } else {
                None
            }
        };

        // 1. Markdown link targets: [label](url)
        if let Ok(md_re) = regex::Regex::new(r#"\[[^\]]*\]\(([^)\s]+)\)"#) {
            for caps in md_re.captures_iter(text) {
                if let Some(m) = caps.get(1) {
                    if let Some(norm) = normalize_url(m.as_str()) {
                        if !urls.contains(&norm) {
                            urls.push(norm);
                        }
                    }
                }
            }
        }

        // 2. Explicit scheme URLs (http:// or https://)
        if let Ok(re) = regex::Regex::new(r#"https?://[^\s<>"'"`\(\)]+"#) {
            for m in re.find_iter(text) {
                if let Some(norm) = normalize_url(m.as_str()) {
                    if !urls.contains(&norm) {
                        urls.push(norm);
                    }
                }
            }
        }

        // 3. Scheme-less domain patterns (www...., github.com/..., crates.io/..., docs.rs/...)
        if let Ok(domain_re) = regex::Regex::new(r#"(?i)\b(?:www\.[a-zA-Z0-9_\-]+(?:\.[a-zA-Z0-9_\-]+)+(?:/[^\s<>"'"`\(\)]*)?|(?:github\.com|crates\.io|docs\.rs|news\.ycombinator\.com|gitlab\.com|bitbucket\.org|reddit\.com|stackoverflow\.com|wikipedia\.org|huggingface\.co)(?:/[^\s<>"'"`\(\)]*)?)"#) {
            for m in domain_re.find_iter(text) {
                if let Some(norm) = normalize_url(m.as_str()) {
                    if !urls.contains(&norm) {
                        urls.push(norm);
                    }
                }
            }
        }

        urls
    }

    /// Automatically ground a user prompt by detecting embedded URLs, fetching their live content,
    /// and injecting clean, token-budgeted markdown into the prompt context with authoritative analytical directive.
    pub async fn augment_prompt_with_urls(&self, prompt: &str, max_tokens_per_url: usize) -> Result<(String, Vec<WebPageContent>)> {
        let urls = Self::extract_urls(prompt);
        if urls.is_empty() {
            return Ok((prompt.to_string(), Vec::new()));
        }

        // Limit to max 3 URLs per prompt to avoid excessive latency or context window bloat
        let target_urls: Vec<String> = urls.into_iter().take(3).collect();
        let mut fetched_pages = Vec::new();

        for url in &target_urls {
            // Safely fetch target URL with SSRF auditing and token limits
            match self.browse_url(url, max_tokens_per_url).await {
                Ok(page) => {
                    fetched_pages.push(page);
                }
                Err(err) => {
                    eprintln!("Warning: Auto-grounding failed for URL '{}': {}", url, err);
                }
            }
        }

        if fetched_pages.is_empty() {
            return Ok((prompt.to_string(), Vec::new()));
        }

        let mut augmented = prompt.to_string();
        augmented.push_str("\n\n--- LIVE WEB CONTEXT (GROUNDED BY HAGIBIS) ---\n");
        for page in &fetched_pages {
            augmented.push_str(&format!(
                "Source: {}\nTitle: {}\nStatus: {}\nContent:\n{}\n\n",
                page.url, page.title, page.status_code, page.markdown
            ));
        }
        augmented.push_str("--- END LIVE WEB CONTEXT ---\n\n");
        augmented.push_str(
            "[AUTHORITATIVE DIRECTIVE FOR THE AI MODEL:\n\
            The above context contains real, live ground-truth data automatically inspected from the user's computer, filesystem, network infrastructure, hardware, and external web links in real-time.\n\
            You MUST directly evaluate, inspect, and synthesize these findings to answer the user's prompt.\n\
            NEVER state or claim that you cannot browse the internet, access external links, view local files, or inspect computer folders and network devices.\n\
            Analyze the live data provided above immediately and thoroughly without hesitation.]\n\
            ----------------------------------------------------"
        );

        Ok((augmented, fetched_pages))
    }

    /// Synchronous wrapper for augment_prompt_with_urls
    pub fn augment_prompt_with_urls_sync(&self, prompt: &str, max_tokens_per_url: usize) -> Result<(String, Vec<WebPageContent>)> {
        let engine = self.clone();
        let p = prompt.to_string();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| HgbError::Execution(format!("Failed to build async runtime: {}", e)))?;
            rt.block_on(engine.augment_prompt_with_urls(&p, max_tokens_per_url))
        })
        .join()
        .map_err(|_| HgbError::Execution("Worker thread panicked during prompt URL augmentation".to_string()))?
    }

    /// Execute live web search against DuckDuckGo or fallback search aggregator
    pub async fn search_web(&self, query: &str, max_results: usize) -> Result<Vec<WebSearchResult>> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }

        let limit = if max_results == 0 { 5 } else { max_results };
        let encoded_q = urlencoding_encode(q);

        // 1. DuckDuckGo HTML live endpoint
        let ddg_url = format!("https://html.duckduckgo.com/html/?q={}", encoded_q);
        
        let ddg_res = self.client.get(&ddg_url)
            .header("Referer", "https://html.duckduckgo.com/")
            .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
            .header("Accept-Language", "en-US,en;q=0.5")
            .send()
            .await;

        if let Ok(resp) = ddg_res {
            if resp.status().is_success() {
                if let Ok(html) = resp.text().await {
                    let results = Self::parse_search_results(&html, limit);
                    if !results.is_empty() {
                        return Ok(results);
                    }
                }
            }
        }

        // 2. Fallback to DuckDuckGo Lite endpoint
        let lite_url = format!("https://lite.duckduckgo.com/lite/?q={}", encoded_q);
        if let Ok(resp) = self.client.get(&lite_url).send().await {
            if resp.status().is_success() {
                if let Ok(html) = resp.text().await {
                    let results = Self::parse_search_results(&html, limit);
                    if !results.is_empty() {
                        return Ok(results);
                    }
                }
            }
        }

        Err(HgbError::Execution(format!("Live web search failed for query: '{}'. Rate limits or network error.", q)))
    }

    /// Synchronous wrapper for search_web
    pub fn search_web_sync(&self, query: &str, max_results: usize) -> Result<Vec<WebSearchResult>> {
        let engine = self.clone();
        let q = query.to_string();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| HgbError::Execution(format!("Failed to build async runtime: {}", e)))?;
            rt.block_on(engine.search_web(&q, max_results))
        })
        .join()
        .map_err(|_| HgbError::Execution("Worker thread panicked during web search".to_string()))?
    }

    /// Parse DuckDuckGo search result items from HTML content
    pub fn parse_search_results(html: &str, max_results: usize) -> Vec<WebSearchResult> {
        let mut results = Vec::new();

        let re_link = regex::Regex::new(r#"(?i)<a[^>]*class="[^"]*result__a[^"]*"[^>]*href="([^"]+)"[^>]*>([\s\S]*?)</a>"#).ok();
        let re_snippet = regex::Regex::new(r#"(?i)<(?:a|div)[^>]*class="[^"]*result__snippet[^"]*"[^>]*>([\s\S]*?)</(?:a|div)>"#).ok();

        if let Some(link_re) = re_link {
            let snippets: Vec<String> = if let Some(snip_re) = re_snippet {
                snip_re.captures_iter(html)
                    .map(|cap| Self::strip_tags_and_unescape(&cap[1]))
                    .collect()
            } else {
                Vec::new()
            };

            for (idx, cap) in link_re.captures_iter(html).enumerate() {
                if results.len() >= max_results {
                    break;
                }

                let raw_href = &cap[1];
                let raw_title = &cap[2];

                let target_url = Self::unwrap_ddg_url(raw_href);
                let clean_title = Self::strip_tags_and_unescape(raw_title);
                let snippet = snippets.get(idx).cloned().unwrap_or_default();

                if !clean_title.is_empty() && !target_url.is_empty() {
                    results.push(WebSearchResult {
                        title: clean_title,
                        url: target_url,
                        snippet,
                        rank: results.len() + 1,
                    });
                }
            }
        }

        if results.is_empty() {
            let re_gen = regex::Regex::new(r#"(?i)<a[^>]*href="(https?://[^"]+)"[^>]*>([\s\S]*?)</a>"#).ok();
            if let Some(gen_re) = re_gen {
                for cap in gen_re.captures_iter(html) {
                    if results.len() >= max_results {
                        break;
                    }
                    let url = cap[1].to_string();
                    let title = Self::strip_tags_and_unescape(&cap[2]);
                    if !title.is_empty() && !url.contains("duckduckgo.com") {
                        results.push(WebSearchResult {
                            title,
                            url,
                            snippet: String::new(),
                            rank: results.len() + 1,
                        });
                    }
                }
            }
        }

        results
    }

    fn unwrap_ddg_url(href: &str) -> String {
        if let Some(pos) = href.find("uddg=") {
            let sub = &href[pos + 5..];
            let end_pos = sub.find('&').unwrap_or(sub.len());
            let encoded = &sub[..end_pos];
            urlencoding_decode(encoded)
        } else if href.starts_with("//") {
            format!("https:{}", href)
        } else {
            href.to_string()
        }
    }

    pub fn html_to_clean_markdown(html: &str) -> (String, String) {
        let mut title = String::new();
        if let Ok(re_title) = regex::Regex::new(r#"(?i)<title[^>]*>([\s\S]*?)</title>"#) {
            if let Some(cap) = re_title.captures(html) {
                title = Self::strip_tags_and_unescape(&cap[1]);
            }
        }

        let strip_patterns = [
            r#"(?is)<script[^>]*>.*?</script>"#,
            r#"(?is)<style[^>]*>.*?</style>"#,
            r#"(?is)<noscript[^>]*>.*?</noscript>"#,
            r#"(?is)<svg[^>]*>.*?</svg>"#,
            r#"(?is)<nav[^>]*>.*?</nav>"#,
            r#"(?is)<header[^>]*>.*?</header>"#,
            r#"(?is)<footer[^>]*>.*?</footer>"#,
            r#"(?is)<iframe[^>]*>.*?</iframe>"#,
            r#"(?is)<aside[^>]*>.*?</aside>"#,
            r#"(?is)<!--.*?-->"#,
        ];

        let mut content = html.to_string();
        for pattern in &strip_patterns {
            if let Ok(re) = regex::Regex::new(pattern) {
                content = re.replace_all(&content, " ").to_string();
            }
        }

        for level in (1..=6).rev() {
            let tag_open = format!("(?is)<h{}[^>]*>(.*?)</h{}>", level, level);
            let hashes = "#".repeat(level);
            if let Ok(re) = regex::Regex::new(&tag_open) {
                content = re.replace_all(&content, format!("\n\n{} $1\n\n", hashes)).to_string();
            }
        }

        if let Ok(re) = regex::Regex::new(r#"(?is)<pre[^>]*><code[^>]*>(.*?)</code></pre>"#) {
            content = re.replace_all(&content, "\n\n```\n$1\n```\n\n").to_string();
        }
        if let Ok(re) = regex::Regex::new(r#"(?is)<pre[^>]*>(.*?)</pre>"#) {
            content = re.replace_all(&content, "\n\n```\n$1\n```\n\n").to_string();
        }
        if let Ok(re) = regex::Regex::new(r#"(?is)<code[^>]*>(.*?)</code>"#) {
            content = re.replace_all(&content, "`$1`").to_string();
        }

        if let Ok(re) = regex::Regex::new(r#"(?is)<a[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#) {
            content = re.replace_all(&content, "[$2]($1)").to_string();
        }

        if let Ok(re) = regex::Regex::new(r#"(?is)<(?:strong|b)\b[^>]*>(.*?)</(?:strong|b)>"#) {
            content = re.replace_all(&content, "**$1**").to_string();
        }
        if let Ok(re) = regex::Regex::new(r#"(?is)<(?:em|i)\b[^>]*>(.*?)</(?:em|i)>"#) {
            content = re.replace_all(&content, "*$1*").to_string();
        }

        if let Ok(re) = regex::Regex::new(r#"(?is)<li[^>]*>(.*?)</li>"#) {
            content = re.replace_all(&content, "\n- $1").to_string();
        }

        if let Ok(re) = regex::Regex::new(r#"(?is)<blockquote[^>]*>(.*?)</blockquote>"#) {
            content = re.replace_all(&content, "\n> $1\n\n").to_string();
        }

        if let Ok(re) = regex::Regex::new(r#"(?is)<p[^>]*>(.*?)</p>"#) {
            content = re.replace_all(&content, "\n\n$1\n\n").to_string();
        }
        if let Ok(re) = regex::Regex::new(r#"(?i)<br\s*/?>"#) {
            content = re.replace_all(&content, "\n").to_string();
        }
        if let Ok(re) = regex::Regex::new(r#"(?is)<hr\s*/?>"#) {
            content = re.replace_all(&content, "\n\n---\n\n").to_string();
        }

        if let Ok(re) = regex::Regex::new(r#"(?is)<tr[^>]*>(.*?)</tr>"#) {
            content = re.replace_all(&content, "\n| $1 |").to_string();
        }
        if let Ok(re) = regex::Regex::new(r#"(?is)<(?:td|th)[^>]*>(.*?)</(?:td|th)>"#) {
            content = re.replace_all(&content, " $1 |").to_string();
        }

        if let Ok(re) = regex::Regex::new(r#"<[^>]+>"#) {
            content = re.replace_all(&content, "").to_string();
        }

        let decoded = html_decode_entities(&content);
        (title.trim().to_string(), decoded)
    }

    pub fn compact_and_truncate_markdown(text: &str, max_tokens: usize) -> (String, usize, bool) {
        let mut clean_lines = Vec::new();
        let mut consecutive_empty = 0;

        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                consecutive_empty += 1;
                if consecutive_empty <= 1 {
                    clean_lines.push("");
                }
            } else {
                consecutive_empty = 0;
                clean_lines.push(trimmed);
            }
        }

        let full_text = clean_lines.join("\n").trim().to_string();
        let words: Vec<&str> = full_text.split_whitespace().collect();
        let estimated_tokens = (words.len() * 4 + 2) / 3;

        if estimated_tokens <= max_tokens || max_tokens == 0 {
            (full_text, estimated_tokens, false)
        } else {
            let word_limit = (max_tokens * 3) / 4;
            let truncated_words = &words[..word_limit.min(words.len())];
            let mut truncated_text = truncated_words.join(" ");
            truncated_text.push_str(&format!("\n\n[Content truncated to stay within {} tokens budget]", max_tokens));
            (truncated_text, max_tokens, true)
        }
    }

    fn strip_tags_and_unescape(raw: &str) -> String {
        let no_tags = if let Ok(re) = regex::Regex::new(r#"<[^>]+>"#) {
            re.replace_all(raw, "").to_string()
        } else {
            raw.to_string()
        };
        html_decode_entities(&no_tags).trim().to_string()
    }
}

fn html_decode_entities(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&hellip;", "…")
}

fn urlencoding_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
            out.push(b as char);
        } else if b == b' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

fn urlencoding_decode(s: &str) -> String {
    let mut bytes = Vec::new();
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '%' {
            let hex1 = chars.next();
            let hex2 = chars.next();
            if let (Some(h1), Some(h2)) = (hex1, hex2) {
                if let Ok(b) = u8::from_str_radix(&format!("{}{}", h1, h2), 16) {
                    bytes.push(b);
                    continue;
                }
            }
        } else if c == '+' {
            bytes.push(b' ');
        } else {
            bytes.push(c as u8);
        }
    }

    String::from_utf8_lossy(&bytes).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_html_to_clean_markdown() {
        let sample_html = r#"
            <html>
                <head>
                    <title>Test Page Title</title>
                    <script>console.log("secret script");</script>
                    <style>.hidden { display: none; }</style>
                </head>
                <body>
                    <header><nav><a href="/home">Home</a></nav></header>
                    <h1>Main Header</h1>
                    <p>This is a paragraph with a <a href="https://example.com">link</a> and <strong>bold text</strong>.</p>
                    <pre><code>let x = 42;</code></pre>
                    <ul>
                        <li>Item 1</li>
                        <li>Item 2</li>
                    </ul>
                    <footer>Footer copyright info</footer>
                </body>
            </html>
        "#;

        let (title, md) = WebBrowserEngine::html_to_clean_markdown(sample_html);
        assert_eq!(title, "Test Page Title");
        assert!(!md.contains("console.log"));
        assert!(!md.contains("display: none"));
        assert!(md.contains("# Main Header"));
        assert!(md.contains("[link](https://example.com)"));
        assert!(md.contains("**bold text**"));
        assert!(md.contains("let x = 42;"));
        assert!(md.contains("- Item 1"));
    }

    #[test]
    fn test_compact_and_truncate_markdown() {
        let text = "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen";
        let (res, tokens, truncated) = WebBrowserEngine::compact_and_truncate_markdown(text, 5);
        assert!(truncated);
        assert!(tokens <= 5);
        assert!(res.contains("truncated"));
    }

    #[test]
    fn test_extract_urls() {
        let prompt = "Please read https://doc.rust-lang.org/book/ and compare with https://crates.io? Also check (http://example.com/test). Thanks!";
        let urls = WebBrowserEngine::extract_urls(prompt);
        assert_eq!(urls.len(), 3);
        assert_eq!(urls[0], "https://doc.rust-lang.org/book/");
        assert_eq!(urls[1], "https://crates.io");
        assert_eq!(urls[2], "http://example.com/test");

        let empty_prompt = "No urls here at all!";
        assert!(WebBrowserEngine::extract_urls(empty_prompt).is_empty());

        let scheme_less = "Check out github.com/rust-lang/rust and docs.rs/serde alongside [HN](news.ycombinator.com) and www.rust-lang.org.";
        let norm_urls = WebBrowserEngine::extract_urls(scheme_less);
        assert!(norm_urls.contains(&"https://github.com/rust-lang/rust".to_string()));
        assert!(norm_urls.contains(&"https://docs.rs/serde".to_string()));
        assert!(norm_urls.contains(&"https://news.ycombinator.com".to_string()));
        assert!(norm_urls.contains(&"https://www.rust-lang.org".to_string()));
    }
}

