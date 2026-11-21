//! Repeatable literal-substring search comparison using the SQLite build shipped with the core.
use std::{fmt::Write as _, fs, path::Path, time::Instant};

use rusqlite::{Connection, params};

const COPIES: usize = 32;
const DOCS: &[&str] = &[
    "字幕缓存按阶段复用。",
    "检查缓存一致性和恢复。",
    "内存容量影响检索速度。",
    "数据库事务保证完整性。",
    "数据采集后写入本地库。",
    "用 SQLite FTS5 搜索中文。",
    "Rust/C++ 处理视频。",
    "示例 C++17 与 C#。",
    "JSON-RPC 使用 task_id 字段。",
    "字段 task-id 与 task id 不同。",
    "代码 foo.bar() 输出结果。",
    "变量 foo_bar 用作占位。",
    "字幕对齐后提取摘要。",
    "一份短词表收录中文检索。",
    "阶段恢复需要显式确认。",
    "标点“引号”、逗号，参与匹配。",
    "查询中含有 \"OR\" 文本。",
    "测量 GPU/CPU 的时间。",
    "预热 GPU 后生成关键帧。",
    "重复字符哈哈哈也要匹配。",
    "只出现一个哈。",
    "排列数据与事务无关。",
    "字节和符号可以共存。",
    "独立的 SQLite3 工具。",
];

struct Query(&'static str, &'static str, &'static [usize]);
const QUERIES: &[Query] = &[
    Query("short", "字", &[0, 8, 9, 12, 19, 22]),
    Query("short", "缓存", &[0, 1]),
    Query("short", "数据", &[3, 4, 21]),
    Query("short", "哈", &[19, 20]),
    Query("short", "恢复", &[1, 14]),
    Query("short", "据库", &[3]),
    Query("chinese", "缓存一致性", &[1]),
    Query("chinese", "中文检索", &[13]),
    Query("chinese", "哈哈哈", &[19]),
    Query("chinese", "不存在", &[]),
    Query("mixed", "sqlite", &[5, 23]),
    Query("mixed", "fts5", &[5]),
    Query("mixed", "rust/c++", &[6]),
    Query("mixed", "GPU/CPU", &[17]),
    Query("mixed", "task_id", &[8]),
    Query("mixed", "foo.bar()", &[10]),
    Query("punct", "++", &[6, 7]),
    Query("punct", "，", &[15]),
    Query("punct", "\"OR\"", &[16]),
    Query("punct", "、逗号，", &[15]),
    Query("punct", "\" OR *", &[]),
    Query("punct", "' OR 1=1 --", &[]),
];

#[derive(Clone, Copy, Debug)]
enum Method {
    Words,
    Bigram,
    Trigram,
}
const METHODS: [Method; 3] = [Method::Words, Method::Bigram, Method::Trigram];

// Small fixed longest-match lexicon; unlisted Chinese characters are single tokens.
const WORDS: &[&str] = &[
    "数据库",
    "一致性",
    "缓存",
    "字幕",
    "事务",
    "中文",
    "检索",
    "恢复",
    "数据",
    "引号",
    "字符",
    "字段",
    "代码",
    "重复",
    "提取",
    "关键帧",
];

fn words(mut text: &str) -> Vec<&str> {
    let mut result = Vec::new();
    while !text.is_empty() {
        let len = WORDS
            .iter()
            .filter(|word| text.starts_with(**word))
            .map(|s| s.len())
            .max()
            .unwrap_or_else(|| {
                let ascii = text.bytes().take_while(u8::is_ascii_alphanumeric).count();
                if ascii > 0 {
                    ascii
                } else {
                    text.chars().next().map_or(0, char::len_utf8)
                }
            });
        let (word, tail) = text.split_at(len);
        result.push(word);
        text = tail;
    }
    result
}

fn indexed(method: Method, text: &str) -> String {
    let text = text.to_ascii_lowercase();
    let tokens = match method {
        Method::Trigram => return text,
        Method::Words => words(&text),
        Method::Bigram => {
            let ends: Vec<_> = text
                .char_indices()
                .map(|(i, _)| i)
                .chain([text.len()])
                .collect();
            ends.windows(3).map(|ix| &text[ix[0]..ix[2]]).collect()
        }
    };
    // Hex preserves punctuation and token boundaries through the built-in ascii tokenizer.
    let mut encoded = String::new();
    for token in tokens {
        for byte in token.bytes() {
            write!(encoded, "{byte:02x}").expect("writing a String");
        }
        encoded.push(' ');
    }
    encoded.pop();
    encoded
}

fn fixture(path: &Path) -> rusqlite::Result<Connection> {
    let mut db = Connection::open(path)?;
    db.execute_batch("PRAGMA page_size=4096; CREATE TABLE segment(body TEXT NOT NULL)")?;
    let tx = db.transaction()?;
    {
        let mut insert = tx.prepare("INSERT INTO segment(body) VALUES (?1)")?;
        for copy in 0..COPIES {
            for doc in DOCS {
                insert.execute([format!("{doc} 样本{copy:03}")])?;
            }
        }
    }
    tx.commit()?;
    Ok(db)
}

fn build(db: &mut Connection, method: Method) -> rusqlite::Result<()> {
    let tx = db.transaction()?;
    tx.execute_batch(match method {
        Method::Trigram => {
            "CREATE VIRTUAL TABLE search USING fts5(tokens, content='', tokenize='trigram')"
        }
        _ => "CREATE VIRTUAL TABLE search USING fts5(tokens, content='', tokenize='ascii')",
    })?;
    {
        let mut insert = tx.prepare("INSERT INTO search(rowid,tokens) VALUES (?1,?2)")?;
        let mut source = tx.prepare("SELECT rowid,body FROM segment")?;
        let mut rows = source.query([])?;
        while let Some(row) = rows.next()? {
            let text: String = row.get(1)?;
            insert.execute(params![row.get::<_, i64>(0)?, indexed(method, &text)])?;
        }
    }
    tx.execute("INSERT INTO search(search) VALUES ('optimize')", [])?;
    tx.commit()
}

fn search(db: &Connection, method: Method, needle: &str) -> rusqlite::Result<Vec<usize>> {
    if needle.is_empty() {
        return Ok(Vec::new());
    }
    let minimum = match method {
        Method::Words => 1,
        Method::Bigram => 2,
        Method::Trigram => 3,
    };
    let (sql, parameter) = if needle.chars().count() < minimum {
        (
            "SELECT rowid FROM segment WHERE instr(lower(body), ?1)>0 ORDER BY rowid",
            needle.to_ascii_lowercase(),
        )
    } else {
        let phrase = indexed(method, needle);
        (
            "SELECT rowid FROM search WHERE search MATCH ?1 ORDER BY rowid",
            format!("\"{}\"", phrase.replace('"', "\"\"")),
        )
    };
    db.prepare(sql)?
        .query_map([parameter], |row| {
            row.get::<_, i64>(0).map(|id| id as usize - 1)
        })?
        .collect()
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Quality {
    hits: usize,
    expected: usize,
    extra: usize,
}

fn check(
    db: &Connection,
    method: Method,
) -> rusqlite::Result<std::collections::BTreeMap<&'static str, Quality>> {
    let mut scores = std::collections::BTreeMap::new();
    for Query(group, needle, labels) in QUERIES {
        let score: &mut Quality = scores.entry(*group).or_default();
        let ids = search(db, method, needle)?;
        let hits = ids
            .iter()
            .filter(|id| labels.contains(&(*id % DOCS.len())))
            .count();
        score.expected += labels.len() * COPIES;
        score.hits += hits;
        score.extra += ids.len() - hits;
    }
    Ok(scores)
}

fn percentile(samples: &mut [u128], percent: usize) -> u128 {
    samples.sort_unstable();
    samples[(samples.len() * percent).div_ceil(100) - 1]
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "SQLite {}, {} segments, {} queries",
        rusqlite::version(),
        DOCS.len() * COPIES,
        QUERIES.len()
    );
    for method in METHODS {
        let mut builds = Vec::new();
        let mut batches = Vec::new();
        for run in 0..5 {
            let dir = tempfile::tempdir()?;
            let path = dir.path().join("search.sqlite3");
            let mut db = fixture(&path)?;
            let before = fs::metadata(&path)?.len();
            let start = Instant::now();
            build(&mut db, method)?;
            builds.push(start.elapsed().as_micros());
            let quality = check(&db, method)?; // Warm the cache and check labels outside the timed loop.
            for _ in 0..10 {
                let start = Instant::now();
                for Query(_, needle, _) in QUERIES {
                    std::hint::black_box(search(&db, method, needle)?);
                }
                batches.push(start.elapsed().as_micros());
            }
            if run == 0 {
                println!(
                    "{method:?} index_bytes={} {quality:?}",
                    fs::metadata(&path)?.len() - before
                );
            }
        }
        println!(
            "build_us p50={} p95={}; hot_batch_us p50={} p95={}",
            percentile(&mut builds, 50),
            percentile(&mut builds, 95),
            percentile(&mut batches, 50),
            percentile(&mut batches, 95)
        );
    }
    Ok(())
}

#[test]
fn literal_queries_match_fixed_labels_and_never_expand_operators() {
    for Query(_, needle, labels) in QUERIES {
        let found: Vec<_> = DOCS
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                s.to_ascii_lowercase()
                    .contains(&needle.to_ascii_lowercase())
            })
            .map(|(i, _)| i)
            .collect();
        assert_eq!(&found, labels, "{needle}");
    }
    for method in METHODS {
        let mut db = fixture(Path::new(":memory:")).unwrap();
        build(&mut db, method).unwrap();
        for (group, score) in check(&db, method).unwrap() {
            assert_eq!(score.extra, 0, "{method:?}/{group}");
            let missed = match (method, group) {
                (Method::Words, "short") => 7 * COPIES,
                (Method::Words, "mixed") => COPIES,
                _ => 0,
            };
            assert_eq!(score.hits, score.expected - missed, "{method:?}/{group}");
        }
        assert!(search(&db, method, "").unwrap().is_empty());
    }
}
