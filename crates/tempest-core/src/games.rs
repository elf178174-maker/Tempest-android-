//! Game discovery.
//!
//! Vortex has no "list my games" endpoint that upstream found, so discovery
//! walks `/api/games/{id}` upward from 1 and stops at the first miss. That is
//! preserved, but made fit for a phone:
//!
//! * requests run concurrently in bounded batches instead of one at a time;
//! * a single gap does not end the walk (upstream stopped at the first 404,
//!   so one delisted game hid every later one);
//! * results are cached to disk so the list appears instantly on next launch
//!   and works offline;
//! * progress is reported so the UI can show a determinate spinner.

use crate::platform::TempestPaths;
use crate::{Result, TempestError};
use serde::{Deserialize, Serialize};

/// How many ids to probe at once. Kept small to stay polite to the server and
/// gentle on a mobile radio.
const BATCH: u32 = 8;
/// Consecutive misses tolerated before concluding the catalogue has ended.
const MISS_TOLERANCE: u32 = 12;
/// Absolute ceiling, so a server that answers 200 to everything cannot spin
/// forever.
const MAX_ID: u32 = 2000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Game {
    pub id: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GameCatalogue {
    pub games: Vec<Game>,
    /// Unix seconds when this snapshot was taken.
    pub fetched_at: u64,
}

impl GameCatalogue {
    /// Case-insensitive substring search over name and description.
    pub fn search(&self, query: &str) -> Vec<&Game> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return self.games.iter().collect();
        }
        self.games
            .iter()
            .filter(|g| {
                g.name.to_lowercase().contains(&q)
                    || g.description
                        .as_deref()
                        .is_some_and(|d| d.to_lowercase().contains(&q))
            })
            .collect()
    }

    pub fn get(&self, id: u32) -> Option<&Game> {
        self.games.iter().find(|g| g.id == id)
    }

    fn cache_file(paths: &TempestPaths) -> std::path::PathBuf {
        paths.cache_dir().join("games.json")
    }

    pub fn load_cached(paths: &TempestPaths) -> Option<Self> {
        let raw = std::fs::read_to_string(Self::cache_file(paths)).ok()?;
        serde_json::from_str(&raw).ok()
    }

    pub fn save(&self, paths: &TempestPaths) -> Result<()> {
        std::fs::create_dir_all(paths.cache_dir())?;
        let json = serde_json::to_string(self).map_err(TempestError::other)?;
        std::fs::write(Self::cache_file(paths), json)?;
        Ok(())
    }
}

/// Parse one `/api/games/{id}` response body.
///
/// Returns `None` for a response that does not describe a real game, which is
/// how the walk detects a gap.
pub fn parse_game(id: u32, body: &serde_json::Value) -> Option<Game> {
    let name = body.get("name")?.as_str()?.trim();
    if name.is_empty() {
        return None;
    }
    Some(Game {
        id,
        name: name.to_string(),
        description: body
            .get("description")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        image_url: ["image_url", "image", "cover", "thumbnail", "banner"]
            .iter()
            .find_map(|k| body.get(*k).and_then(|v| v.as_str()))
            .filter(|s| s.starts_with("http") || s.starts_with('/'))
            .map(|s| {
                if s.starts_with('/') {
                    format!("{}{s}", crate::auth::BASE)
                } else {
                    s.to_string()
                }
            }),
    })
}

/// Fetch a single game by id.
pub async fn fetch_one(
    client: &reqwest::Client,
    token: &str,
    id: u32,
) -> Result<Option<Game>> {
    let resp = client
        .get(crate::auth::game_api_url(id))
        .header("Cookie", crate::auth::session_cookie(token))
        .send()
        .await?;

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(TempestError::Auth(
            "your Vortex session has expired — sign in again".into(),
        ));
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(TempestError::Network(format!("HTTP {status} for game {id}")));
    }

    let body: serde_json::Value = resp.json().await?;
    Ok(parse_game(id, &body))
}

/// Walk the catalogue. `progress(found_so_far, highest_id_probed)`.
pub async fn discover(
    token: &str,
    cancel: &crate::net::CancelToken,
    progress: Option<&(dyn Fn(usize, u32) + Send + Sync)>,
) -> Result<GameCatalogue> {
    let client = crate::net::client()?;
    let mut games: Vec<Game> = Vec::new();
    let mut consecutive_misses = 0u32;
    let mut next_id = 1u32;

    while next_id <= MAX_ID && consecutive_misses < MISS_TOLERANCE {
        if cancel.is_cancelled() {
            return Err(TempestError::Cancelled);
        }

        let ids: Vec<u32> = (next_id..next_id + BATCH).collect();
        let results = futures_util::future::join_all(
            ids.iter().map(|id| fetch_one(&client, token, *id)),
        )
        .await;

        for (id, result) in ids.iter().zip(results) {
            match result {
                Ok(Some(game)) => {
                    games.push(game);
                    consecutive_misses = 0;
                }
                Ok(None) => consecutive_misses += 1,
                // An auth failure is terminal; a transient network error for one
                // id should not abandon the whole catalogue.
                Err(e @ TempestError::Auth(_)) => return Err(e),
                Err(e) => {
                    crate::logging::warn("games", format!("game {id}: {e}"));
                    consecutive_misses += 1;
                }
            }
        }

        next_id += BATCH;
        if let Some(cb) = progress {
            cb(games.len(), next_id.saturating_sub(1));
        }
    }

    games.sort_by_key(|g| g.id);
    games.dedup_by_key(|g| g.id);

    Ok(GameCatalogue {
        games,
        fetched_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_a_full_game_record() {
        let g = parse_game(3, &json!({
            "name": "Test Game",
            "description": "A game for testing",
            "image_url": "https://cdn.example/img.png"
        }))
        .unwrap();
        assert_eq!(g.id, 3);
        assert_eq!(g.name, "Test Game");
        assert_eq!(g.description.as_deref(), Some("A game for testing"));
        assert_eq!(g.image_url.as_deref(), Some("https://cdn.example/img.png"));
    }

    #[test]
    fn relative_image_paths_are_resolved_against_the_vortex_origin() {
        let g = parse_game(1, &json!({"name": "G", "image": "/static/g.png"})).unwrap();
        assert_eq!(g.image_url.as_deref(), Some("https://playvortex.io/static/g.png"));
    }

    #[test]
    fn a_record_without_a_usable_name_is_not_a_game() {
        assert!(parse_game(1, &json!({})).is_none());
        assert!(parse_game(1, &json!({"name": ""})).is_none());
        assert!(parse_game(1, &json!({"name": "   "})).is_none());
        assert!(parse_game(1, &json!({"name": 42})).is_none());
    }

    #[test]
    fn javascript_urls_in_image_fields_are_ignored() {
        let g = parse_game(1, &json!({"name": "G", "image": "javascript:alert(1)"})).unwrap();
        assert_eq!(g.image_url, None);
    }

    fn catalogue() -> GameCatalogue {
        GameCatalogue {
            games: vec![
                Game { id: 1, name: "Half-Life".into(), description: Some("shooter".into()), image_url: None },
                Game { id: 2, name: "Portal".into(), description: None, image_url: None },
                Game { id: 3, name: "Team Fortress".into(), description: Some("A shooter too".into()), image_url: None },
            ],
            fetched_at: 0,
        }
    }

    #[test]
    fn search_is_case_insensitive_and_matches_descriptions() {
        let c = catalogue();
        assert_eq!(c.search("portal").len(), 1);
        assert_eq!(c.search("PORTAL")[0].id, 2);
        assert_eq!(c.search("shooter").len(), 2, "should match both descriptions");
        assert_eq!(c.search("  ").len(), 3, "blank query returns everything");
        assert!(c.search("nothing here").is_empty());
    }

    #[test]
    fn lookup_by_id() {
        let c = catalogue();
        assert_eq!(c.get(2).unwrap().name, "Portal");
        assert!(c.get(99).is_none());
    }

    #[test]
    fn catalogue_survives_a_disk_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let paths = TempestPaths::with_root(dir.path(), dir.path());
        let c = catalogue();
        c.save(&paths).unwrap();
        let loaded = GameCatalogue::load_cached(&paths).unwrap();
        assert_eq!(loaded.games, c.games);
    }

    #[test]
    fn missing_cache_is_none_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let paths = TempestPaths::with_root(dir.path(), dir.path());
        assert!(GameCatalogue::load_cached(&paths).is_none());
    }
}
