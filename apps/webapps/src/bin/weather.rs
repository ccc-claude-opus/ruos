//! weather: the weather now in a few cities, from Open-Meteo (`http`),
//! kept 10 minutes in sql:weather so the outside service is asked at most
//! once per city per 10 minutes, whichever instance answers.
//!   GET /                       the page
//!   GET /api/cities             the cities
//!   GET /api/weather?city=ID    {"city", "temperature", "wind", "at", "cached"}
//! The first argument replaces the service's address
//! (https://api.open-meteo.com; the tests give a local one).
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;

use libsys::http::Http;
use webapps::*;

libsys::entry!(main);

/// id, name, latitude, longitude.
const CITIES: &[(&str, &str, &str, &str)] = &[
    ("taipei", "台北", "25.03", "121.57"),
    ("kinmen", "金門", "24.43", "118.32"),
    ("kaohsiung", "高雄", "22.63", "120.30"),
    ("tokyo", "東京", "35.68", "139.69"),
    ("london", "倫敦", "51.51", "-0.13"),
    ("newyork", "紐約", "40.71", "-74.01"),
];

const BODY: &str = r##"<h1>天氣</h1><p class="sub">函式經 <code>http</code> 問 Open-Meteo，結果在後端的 SQLite（<code>sql:weather</code>）存 10 分鐘。</p>
<div id="cities"></div>"##;

const SCRIPT: &str = r##"
(async () => {
  const cities = await api('GET', 'api/cities');
  const box = document.getElementById('cities');
  box.innerHTML = cities.map((c) => `<div class="card" id="c-${c.id}"><b>${esc(c.name)}</b> <span class="muted">讀取中…</span></div>`).join('');
  for (const c of cities) {
    const el = document.getElementById(`c-${c.id}`);
    try {
      const w = await api('GET', `api/weather?city=${c.id}`);
      el.innerHTML = `<b>${esc(c.name)}</b> ${w.temperature} °C，風速 ${w.wind} km/h <span class="muted">（${esc(w.at)}${w.cached ? '，快取' : ''}）</span>`;
    } catch (err) { el.innerHTML = `<b>${esc(c.name)}</b> <span class="err">${esc(err.message)}</span>`; }
  }
})();
"##;

fn main(_arg: usize) -> i64 {
    let (args, _) = libsys::args::load();
    let source = args.get(1).map(|a| String::from_utf8_lossy(a).into_owned()).unwrap_or_else(|| String::from("https://api.open-meteo.com"));
    let http = Http::get();
    let db = Db::get("weather");
    let _ = db.run("CREATE TABLE IF NOT EXISTS cache (city TEXT PRIMARY KEY, temperature REAL, wind REAL, at TEXT, fetched INTEGER)", &[]);
    serve_http(|req| match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") => page("天氣", BODY, SCRIPT),
        ("GET", "/api/cities") => ok(Value::from(CITIES.iter().map(|c| json!({ "id": c.0, "name": c.1 })).collect::<alloc::vec::Vec<_>>())),
        ("GET", "/api/weather") => {
            let id = param(req, "city").unwrap_or_default();
            let Some(&(id, name, lat, lon)) = CITIES.iter().find(|c| c.0 == id) else { return error(404, "no such city") };
            // Fresh enough in the cache?
            let fresh = db.row(
                "SELECT temperature, wind, at FROM cache WHERE city = ?1 AND fetched > CAST(strftime('%s', 'now') AS INTEGER) - 600",
                &[json!(id)],
            );
            if let Ok(Some(c)) = fresh {
                return ok(json!({ "city": name, "temperature": c["temperature"], "wind": c["wind"], "at": c["at"], "cached": true }));
            }
            let Some(http) = &http else { return error(500, "no http (add it to the manifest's uses)") };
            let url = format!("{source}/v1/forecast?latitude={lat}&longitude={lon}&current=temperature_2m,wind_speed_10m&timezone=auto");
            let r = match http.fetch("GET", &url, "", &[]) {
                Ok(Ok(r)) if r.status == 200 => r,
                Ok(Ok(r)) => return error(502, &format!("the weather service said {}", r.status)),
                Ok(Err(why)) => return error(502, &why),
                Err(e) => return error(502, &format!("{e:?}")),
            };
            let j: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
            let cur = &j["current"];
            let (Some(t), Some(w)) = (cur["temperature_2m"].as_f64(), cur["wind_speed_10m"].as_f64()) else {
                return error(502, "the weather service's answer has no current weather");
            };
            let at = String::from(cur["time"].as_str().unwrap_or(""));
            let _ = db.run(
                "INSERT OR REPLACE INTO cache VALUES (?1, ?2, ?3, ?4, CAST(strftime('%s', 'now') AS INTEGER))",
                &[json!(id), json!(t), json!(w), json!(at)],
            );
            ok(json!({ "city": name, "temperature": t, "wind": w, "at": at, "cached": false }))
        }
        _ => error(404, "no such page"),
    })
}
