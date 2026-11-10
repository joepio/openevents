//! openevents-sim: sends a steady stream of semi-realistic events to an OpenEvents in
//! dev mode (OPENEVENTS_DEV_AUTH=true), using the example profiles `nl.vng.meldingen`,
//! `nl.vng.vergunningen` and `nl.vng.gesprekken`.
//!
//! Every case (a melding, a vergunningaanvraag, a gesprek) is a script of events that is
//! played step by step, interleaved with other cases. Gesprekken are linked to their zaak
//! via `aanleiding.zaak`. Actors post with their own dev token, so the read and write
//! rules of the profiles apply exactly as for real clients.
//!
//! Usage: openevents-sim [--base URL] [--rate EVENTS_PER_SEC] [--count N] [--active N] [--seed N]

use chrono::{Duration as Days, Local, Utc};
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::time::Duration;

const ID: &str = "https://id.example.nl/";

#[derive(Clone)]
struct Actor {
    id: String,
    name: String,
    claims: Vec<(&'static str, &'static str)>,
}

impl Actor {
    fn new(name: &str, claims: &[(&'static str, &'static str)]) -> Actor {
        Actor {
            id: format!("{ID}{name}"),
            name: name.to_string(),
            claims: claims.to_vec(),
        }
    }
    fn module(id: &str, claims: &[(&'static str, &'static str)]) -> Actor {
        Actor {
            id: id.to_string(),
            name: id.rsplit(':').next().unwrap_or(id).to_string(),
            claims: claims.to_vec(),
        }
    }
    fn token(&self) -> String {
        let mut t = format!("dev:{}", self.id);
        for (k, v) in &self.claims {
            t.push_str(&format!("|{k}={v}"));
        }
        t
    }
    fn team(&self) -> &'static str {
        self.claims
            .iter()
            .find(|(k, _)| *k == "team")
            .map(|(_, v)| *v)
            .unwrap_or("")
    }
}

struct Ev {
    actor: Actor,
    source: &'static str,
    kind: String,
    subject: String,
    label: String,
    data: Value,
}

enum Item {
    Ev(Ev),
    Spawn(Case),
}

struct Case {
    items: VecDeque<Item>,
}

struct World {
    rng: StdRng,
    inwoners: Vec<Actor>,
    personas_inwoner: Vec<Actor>,
    teams: Vec<(&'static str, Vec<Actor>)>,
    kcc: Vec<Actor>,
    router: Actor,
    nr: u32,
}

const STRATEN: &[(&str, &str, f64, f64)] = &[
    ("Oudegracht", "Centrum", 5.1196, 52.0894),
    ("Lauwerecht", "Noordoost", 5.1190, 52.1010),
    ("Biltstraat", "Oost", 5.1325, 52.0935),
    ("Burgemeester Reigerstraat", "Oost", 5.1350, 52.0880),
    ("Kardinaal de Jongweg", "Noordoost", 5.1390, 52.1060),
    ("Kanaalstraat", "West", 5.1035, 52.0905),
    ("Vleutenseweg", "West", 5.0920, 52.0915),
    ("Amsterdamsestraatweg", "Noordwest", 5.0995, 52.1060),
    ("Roelantdreef", "Overvecht", 5.1150, 52.1210),
    ("Rijnlaan", "Zuid", 5.1080, 52.0740),
    ("Columbuslaan", "Zuidwest", 5.0890, 52.0780),
    ("Grauwaart", "Leidsche Rijn", 5.0450, 52.1000),
];

const MELDINGEN: &[(&str, &[&str])] = &[
    (
        "afval",
        &[
            "Volle container, er staan zakken naast.",
            "Grofvuil op de stoep gezet, al drie dagen.",
            "Ondergrondse container zit vast, klep gaat niet open.",
        ],
    ),
    (
        "verlichting",
        &[
            "Lantaarnpaal brandt niet, erg donker 's avonds.",
            "Straatlantaarn knippert de hele nacht.",
        ],
    ),
    (
        "wegdek",
        &[
            "Diep gat in het fietspad, gevaarlijk.",
            "Losliggende stoeptegels voor de ingang van de school.",
        ],
    ),
    (
        "groen",
        &[
            "Grote tak afgebroken na de storm, ligt over het pad.",
            "Heg groeit over het trottoir, rolstoel kan er niet langs.",
        ],
    ),
    (
        "overlast",
        &[
            "Geluidsoverlast van een feest, elke vrijdagnacht.",
            "Hangjongeren bij het speeltuintje, veel rommel.",
        ],
    ),
    (
        "fietswrak",
        &["Fietswrak al maanden aan het hek, zonder voorwiel."],
    ),
    (
        "graffiti",
        &[
            "Graffiti op de muur van het buurthuis.",
            "Tags op de bushalte.",
        ],
    ),
];

const AANVRAGEN: &[(&[&str], &str, &str)] = &[
    (&["bouwen"], "Dakkapel aan de voorzijde", "regulier"),
    (
        &["bouwen"],
        "Uitbouw aan de achterzijde van de woning",
        "regulier",
    ),
    (
        &["kappen"],
        "Kappen van een eik in de achtertuin",
        "regulier",
    ),
    (
        &["uitrit"],
        "Aanleg van een uitrit naar de oprit",
        "regulier",
    ),
    (
        &["bouwen", "monument"],
        "Vervangen kozijnen van een gemeentelijk monument",
        "regulier",
    ),
    (
        &["gebruik"],
        "Bovenwoning gebruiken als twee appartementen",
        "uitgebreid",
    ),
    (
        &["bouwen", "kappen"],
        "Nieuwbouw garage, daarvoor twee bomen kappen",
        "regulier",
    ),
];

const DOCUMENTEN: &[&str] = &[
    "de constructieberekening",
    "een situatietekening",
    "foto's van de huidige situatie",
    "een bomeneffectanalyse",
];

const WMO: &[(&str, &str, &str)] = &[
    (
        "Vraag over mijn traplift",
        "Wanneer komt de monteur voor de traplift langs?",
        "Ik heb het nagevraagd, de monteur komt donderdag tussen 9 en 12 uur.",
    ),
    (
        "Huishoudelijke hulp",
        "Mijn hulp is al twee weken niet geweest. Klopt dat?",
        "Excuses, er was een wisseling bij de zorgaanbieder. Vanaf maandag komt er weer iemand.",
    ),
    (
        "Scootmobiel",
        "De accu van mijn scootmobiel gaat niet meer lang mee.",
        "Ik heb een reparatieverzoek gedaan bij de leverancier, zij bellen u.",
    ),
];

fn naam(a: &Actor) -> String {
    let mut c = a.name.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

impl World {
    fn new(seed: u64) -> World {
        let inw = |n: &str| Actor::new(n, &[("rol", "inwoner")]);
        let med =
            |n: &str, team: &'static str| Actor::new(n, &[("rol", "medewerker"), ("team", team)]);
        World {
            rng: StdRng::seed_from_u64(seed),
            personas_inwoner: vec![inw("suzanne"), inw("karel"), inw("pieter")],
            inwoners: [
                "anouk", "bram", "fenna", "hamza", "ilse", "jesse", "lotte", "mohamed", "noor",
                "ruben", "sanne", "thijs", "yara", "zeynep", "wouter", "esra",
            ]
            .iter()
            .map(|n| inw(n))
            .collect(),
            teams: vec![
                (
                    "buitenruimte",
                    vec![
                        med("daan", "buitenruimte"),
                        med("joris", "buitenruimte"),
                        med("linda", "buitenruimte"),
                    ],
                ),
                (
                    "reiniging",
                    vec![med("ahmed", "reiniging"), med("petra", "reiniging")],
                ),
                (
                    "handhaving",
                    vec![med("bas", "handhaving"), med("mirjam", "handhaving")],
                ),
                (
                    "vergunningen",
                    vec![
                        med("fatima", "vergunningen"),
                        med("erik", "vergunningen"),
                        med("sophie", "vergunningen"),
                    ],
                ),
                ("wmo", vec![med("merve", "wmo"), med("ellen", "wmo")]),
            ],
            kcc: vec![
                Actor::new("kim", &[("rol", "kcc")]),
                Actor::new("tom", &[("rol", "kcc")]),
            ],
            router: Actor::module("urn:module:meldingen-routering", &[("rol", "regie")]),
            nr: 0,
        }
    }

    /// One in three cases is for a persona, so the UI personas have something to see.
    fn inwoner(&mut self) -> Actor {
        if self.rng.gen_bool(0.35) {
            self.personas_inwoner.choose(&mut self.rng).unwrap().clone()
        } else {
            self.inwoners.choose(&mut self.rng).unwrap().clone()
        }
    }

    /// Personas (first in each team) get half of the work.
    fn medewerker(&mut self, team: &str) -> Actor {
        let list = &self.teams.iter().find(|(t, _)| *t == team).unwrap().1;
        if self.rng.gen_bool(0.5) {
            list[0].clone()
        } else {
            list.choose(&mut self.rng).unwrap().clone()
        }
    }

    fn zaaknummer(&mut self, prefix: &str) -> String {
        self.nr += 1;
        format!(
            "{prefix}-{}-{:05}",
            Local::now().format("%Y"),
            self.rng.gen_range(10_000..90_000) + self.nr
        )
    }

    fn plek(&mut self) -> (String, String, Value) {
        let (straat, wijk, lon, lat) = *STRATEN.choose(&mut self.rng).unwrap();
        let nr = self.rng.gen_range(1..180);
        let lon = lon + self.rng.gen_range(-0.0015..0.0015);
        let lat = lat + self.rng.gen_range(-0.0010..0.0010);
        let r = |x: f64| (x * 1e6).round() / 1e6;
        (
            format!("{straat} {nr}"),
            wijk.to_string(),
            json!({ "type": "Point", "coordinates": [r(lon), r(lat)] }),
        )
    }

    fn new_case(&mut self) -> Case {
        match self.rng.gen_range(0..100) {
            0..=54 => self.melding(),
            55..=84 => self.vergunning(),
            _ => self.wmo_gesprek(),
        }
    }

    fn melding(&mut self) -> Case {
        let subject = format!("urn:uuid:{}", uuid::Uuid::now_v7());
        let nummer = self.zaaknummer("MOR");
        let melder = self.inwoner();
        let (categorie, teksten) = *MELDINGEN.choose(&mut self.rng).unwrap();
        let omschrijving = teksten.choose(&mut self.rng).unwrap().to_string();
        let team = match categorie {
            "afval" | "fietswrak" => "reiniging",
            "overlast" => "handhaving",
            _ => "buitenruimte",
        };
        let (adres, wijk, locatie) = self.plek();
        let via_kcc = self.rng.gen_bool(0.2);
        let (indiener, kanaal) = if via_kcc {
            (self.kcc.choose(&mut self.rng).unwrap().clone(), "telefoon")
        } else {
            (
                melder.clone(),
                *["app", "website"].choose(&mut self.rng).unwrap(),
            )
        };
        let behandelaar = self.medewerker(team);
        let m = "nl.vng.meldingen.melding";
        let ev = |actor: &Actor, source, t: &str, data: Value| {
            Item::Ev(Ev {
                actor: actor.clone(),
                source,
                kind: format!("{m}.{t}"),
                subject: subject.clone(),
                label: nummer.clone(),
                data,
            })
        };
        let mut items = VecDeque::new();
        items.push_back(ev(
            &indiener,
            "urn:sim:mijnomgeving",
            "gedaan",
            json!({
                "zaaknummer": nummer, "categorie": categorie, "omschrijving": omschrijving,
                "locatie": locatie, "adres": adres, "wijk": wijk, "kanaal": kanaal,
                "melder": { "partij": melder.id, "rol": "inwoner" }
            }),
        ));
        items.push_back(ev(
            &self.router,
            "urn:sim:routering",
            "toegewezen",
            json!({ "team": team }),
        ));
        items.push_back(ev(
            &behandelaar,
            "urn:sim:zaaksysteem",
            "in_behandeling_genomen",
            json!({ "behandelaar": behandelaar.id }),
        ));
        if self.rng.gen_bool(0.3) {
            let vraag = format!(
                "Ik heb een melding gedaan over {adres} ({categorie}). Wanneer wordt dit opgepakt?"
            );
            let antwoord = "Bedankt voor uw bericht. Een collega gaat er deze week naartoe.";
            items.push_back(Item::Spawn(self.gesprek(
                &melder,
                &behandelaar,
                true,
                &format!("Vraag over melding {nummer}"),
                Some(&subject),
                &[
                    (true, &vraag),
                    (false, antwoord),
                    (true, "Fijn, dank u wel!"),
                ],
                "Vraag beantwoord",
            )));
        }
        items.push_back(ev(
            &behandelaar,
            "urn:sim:zaaksysteem",
            "opgelost",
            json!({ "toelichting": "Ter plaatse opgelost." }),
        ));
        if self.rng.gen_bool(0.12) {
            items.push_back(ev(
                &melder,
                "urn:sim:mijnomgeving",
                "heropend",
                json!({ "reden": "Het probleem is er nog steeds." }),
            ));
            items.push_back(ev(
                &behandelaar,
                "urn:sim:zaaksysteem",
                "in_behandeling_genomen",
                json!({ "behandelaar": behandelaar.id }),
            ));
            items.push_back(ev(
                &behandelaar,
                "urn:sim:zaaksysteem",
                "opgelost",
                json!({ "toelichting": "Opnieuw bekeken en nu echt opgelost." }),
            ));
        }
        items.push_back(ev(
            &self.router,
            "urn:sim:routering",
            "afgesloten",
            json!({ "reden": "Automatisch afgesloten na opgelost." }),
        ));
        Case { items }
    }

    fn vergunning(&mut self) -> Case {
        let subject = format!("urn:uuid:{}", uuid::Uuid::now_v7());
        let nummer = self.zaaknummer("OV");
        let aanvrager = self.inwoner();
        let (activiteiten, omschrijving, procedure) = *AANVRAGEN.choose(&mut self.rng).unwrap();
        let (adres, _wijk, locatie) = self.plek();
        let behandelaar = self.medewerker("vergunningen");
        let today = Utc::now().date_naive();
        let weken = if procedure == "uitgebreid" { 26 } else { 8 };
        let beslis = today + Days::weeks(weken);
        let v = "nl.vng.vergunningen.aanvraag";
        let ev = |actor: &Actor, source, t: &str, data: Value| {
            Item::Ev(Ev {
                actor: actor.clone(),
                source,
                kind: format!("{v}.{t}"),
                subject: subject.clone(),
                label: nummer.clone(),
                data,
            })
        };
        let mut items = VecDeque::new();
        items.push_back(ev(
            &aanvrager,
            "urn:sim:omgevingsloket",
            "ingediend",
            json!({
                "zaaknummer": nummer, "activiteiten": activiteiten, "omschrijving": omschrijving,
                "procedure": procedure, "locatie": locatie, "adres": adres,
                "verzoeknummer": format!("{}", self.rng.gen_range(1_000_000..9_999_999)),
                "aanvrager": { "partij": aanvrager.id, "rol": "aanvrager" }
            }),
        ));
        items.push_back(ev(
            &behandelaar,
            "urn:sim:zaaksysteem",
            "behandelaar_toegewezen",
            json!({ "behandelaar": behandelaar.id }),
        ));
        if self.rng.gen_bool(0.4) {
            let doc = *DOCUMENTEN.choose(&mut self.rng).unwrap();
            let toelichting = format!("Wij missen nog {doc}.");
            items.push_back(ev(
                &behandelaar,
                "urn:sim:zaaksysteem",
                "aanvulling_gevraagd",
                json!({ "toelichting": toelichting, "termijnTot": (today + Days::days(14)).to_string() }),
            ));
            let vraag = format!(
                "Voor de beoordeling van uw aanvraag missen we nog {doc}. Kunt u die binnen twee weken aanleveren?"
            );
            items.push_back(Item::Spawn(self.gesprek(
                &aanvrager,
                &behandelaar,
                false,
                &format!("Aanvulling nodig voor {nummer}"),
                Some(&subject),
                &[
                    (false, &vraag),
                    (
                        true,
                        "Ik vraag het na bij mijn architect en stuur het deze week.",
                    ),
                    (
                        true,
                        "Ik heb de stukken net geüpload in het Omgevingsloket.",
                    ),
                    (
                        false,
                        "Ontvangen, dank u. We nemen de aanvraag weer in behandeling.",
                    ),
                ],
                "Aanvulling ontvangen",
            )));
            items.push_back(ev(
                &aanvrager,
                "urn:sim:omgevingsloket",
                "aanvulling_ontvangen",
                json!({ "documenten": [doc] }),
            ));
        }
        items.push_back(ev(
            &behandelaar,
            "urn:sim:zaaksysteem",
            "ontvankelijk_verklaard",
            json!({ "uiterlijkeBeslisdatum": beslis.to_string() }),
        ));
        if activiteiten.contains(&"bouwen") || activiteiten.contains(&"monument") {
            let uitkomst = *[
                "positief",
                "positief",
                "positief met voorwaarden",
                "negatief",
            ]
            .choose(&mut self.rng)
            .unwrap();
            items.push_back(ev(
                &behandelaar,
                "urn:sim:zaaksysteem",
                "advies_ontvangen",
                json!({ "adviseur": "Commissie ruimtelijke kwaliteit", "uitkomst": uitkomst }),
            ));
        }
        if self.rng.gen_bool(0.2) {
            items.push_back(ev(
                &behandelaar,
                "urn:sim:zaaksysteem",
                "termijn_verlengd",
                json!({ "uiterlijkeBeslisdatum": (beslis + Days::weeks(6)).to_string(), "reden": "Advies nodig van een externe partij." }),
            ));
        }
        let uitkomst = match self.rng.gen_range(0..100) {
            0..=59 => "verleend",
            60..=84 => "verleend met voorschriften",
            85..=94 => "geweigerd",
            _ => "buiten behandeling",
        };
        items.push_back(ev(
            &behandelaar,
            "urn:sim:zaaksysteem",
            "besloten",
            json!({ "uitkomst": uitkomst }),
        ));
        if uitkomst.starts_with("verleend") {
            items.push_back(ev(
                &behandelaar,
                "urn:sim:zaaksysteem",
                "bekendgemaakt",
                json!({ "publicatie": format!("https://zoek.officielebekendmakingen.nl/gmb-{}-{}", Local::now().format("%Y"), self.rng.gen_range(100_000..999_999)) }),
            ));
        }
        Case { items }
    }

    fn wmo_gesprek(&mut self) -> Case {
        let inwoner = self.inwoner();
        let mw = self.medewerker("wmo");
        let (onderwerp, vraag, antwoord) = *WMO.choose(&mut self.rng).unwrap();
        self.gesprek(
            &inwoner,
            &mw,
            true,
            onderwerp,
            None,
            &[(true, vraag), (false, antwoord), (true, "Dank u wel.")],
            "Vraag beantwoord",
        )
    }

    /// A gesprek between an inwoner and a medewerker. The medewerker adds the other
    /// party (write right via the team), then they take turns; the medewerker closes it.
    #[allow(clippy::too_many_arguments)]
    fn gesprek(
        &mut self,
        inwoner: &Actor,
        medewerker: &Actor,
        door_inwoner: bool,
        onderwerp: &str,
        zaak: Option<&str>,
        script: &[(bool, &str)],
        reden: &str,
    ) -> Case {
        let subject = format!("urn:uuid:{}", uuid::Uuid::now_v7());
        let label = onderwerp.to_string();
        let g = "nl.vng.gesprekken";
        let ev = |actor: &Actor, t: &str, data: Value| {
            Item::Ev(Ev {
                actor: actor.clone(),
                source: "urn:sim:mijngesprekken",
                kind: format!("{g}.{t}"),
                subject: subject.clone(),
                label: label.clone(),
                data,
            })
        };
        let team = medewerker.team();
        let mut items = VecDeque::new();
        let mut gestart = json!({ "onderwerp": onderwerp, "team": team });
        if let Some(z) = zaak {
            gestart["aanleiding"] = json!({ "zaak": z });
        }
        let mut script: VecDeque<(bool, &str)> = script.iter().copied().collect();
        if door_inwoner {
            gestart["initiatiefnemer"] = json!({ "partij": inwoner.id, "rol": "inwoner" });
            items.push_back(ev(inwoner, "gesprek.gestart", gestart));
            if let Some((true, t)) = script.front().copied() {
                items.push_back(ev(inwoner, "bijdrage.toegevoegd", json!({ "tekst": t })));
                script.pop_front();
            }
            items.push_back(ev(
                medewerker,
                "deelname.toegevoegd",
                json!({ "partij": medewerker.id, "rol": "behandelaar" }),
            ));
        } else {
            gestart["initiatiefnemer"] = json!({ "partij": medewerker.id, "rol": "behandelaar" });
            items.push_back(ev(medewerker, "gesprek.gestart", gestart));
            items.push_back(ev(
                medewerker,
                "deelname.toegevoegd",
                json!({ "partij": inwoner.id, "rol": "inwoner" }),
            ));
        }
        for (van_inwoner, tekst) in script {
            let a = if van_inwoner { inwoner } else { medewerker };
            items.push_back(ev(
                a,
                "bijdrage.toegevoegd",
                json!({ "tekst": tekst, "automatisch": false }),
            ));
        }
        items.push_back(ev(
            medewerker,
            "gesprek.afgesloten",
            json!({ "reden": reden }),
        ));
        Case { items }
    }
}

fn post(agent: &ureq::Agent, base: &str, e: &Ev) -> Result<(), String> {
    let body = json!({
        "specversion": "1.0",
        "id": uuid::Uuid::now_v7().to_string(),
        "source": e.source,
        "type": e.kind,
        "subject": e.subject,
        "time": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "datacontenttype": "application/json",
        "data": e.data,
    });
    match agent
        .post(&format!("{base}/events"))
        .set("authorization", &format!("Bearer {}", e.actor.token()))
        .send_json(body)
    {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(code, r)) => Err(format!(
            "{code} {}",
            r.into_string()
                .unwrap_or_default()
                .chars()
                .take(300)
                .collect::<String>()
        )),
        Err(e) => Err(e.to_string()),
    }
}

fn arg<T: std::str::FromStr>(args: &[String], name: &str, default: T) -> T {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "openevents-sim [--base URL] [--rate EVENTS_PER_SEC] [--count N] [--active N] [--seed N]\n\n\
             Sends semi-realistic events (meldingen, vergunningaanvragen, gesprekken) to an\n\
             OpenEvents in dev mode with the example profiles installed.\n\n\
             --base    default http://127.0.0.1:8080\n\
             --rate    events per second, default 1 (0 = as fast as possible)\n\
             --count   stop after N events, default endless\n\
             --active  cases running at the same time, default 8\n\
             --seed    random seed, default: time based"
        );
        return;
    }
    let base: String = arg(&args, "--base", "http://127.0.0.1:8080".to_string());
    let base = base.trim_end_matches('/').to_string();
    let rate: f64 = arg(&args, "--rate", 1.0);
    let count: u64 = arg(&args, "--count", u64::MAX);
    let active: usize = arg(&args, "--active", 8).max(1);
    let seed: u64 = arg(&args, "--seed", Utc::now().timestamp_millis() as u64);

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(10))
        .build();
    match agent.get(&format!("{base}/profiles")).call() {
        Ok(r) => {
            let body: Value = r.into_json().unwrap_or(Value::Null);
            let text = body.to_string();
            for p in [
                "nl.vng.meldingen",
                "nl.vng.vergunningen",
                "nl.vng.gesprekken",
            ] {
                if !text.contains(&format!("\"{p}\"")) {
                    eprintln!("Profiel {p} ontbreekt. Start de server met OPENEVENTS_PROFILES=examples/profiles.");
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("Kan {base} niet bereiken: {e}");
            std::process::exit(1);
        }
    }
    eprintln!("openevents-sim: {base}, {rate} events/s, seed {seed}");

    let mut w = World::new(seed);
    let mut cases: Vec<Case> = Vec::new();
    let mut sent = 0u64;
    let mut failed = 0u64;
    while sent < count {
        // Keep about `active` zaken running; spawned gesprekken come on top.
        if cases.len() < active && (cases.is_empty() || w.rng.gen_bool(0.35)) {
            cases.push(w.new_case());
        }
        let i = w.rng.gen_range(0..cases.len());
        let mut ev = None;
        while let Some(item) = cases[i].items.pop_front() {
            match item {
                Item::Spawn(c) => cases.push(c),
                Item::Ev(e) => {
                    ev = Some(e);
                    break;
                }
            }
        }
        let Some(e) = ev else {
            cases.swap_remove(i);
            continue;
        };
        match post(&agent, &base, &e) {
            Ok(()) => {
                sent += 1;
                println!(
                    "{} {:<52} {:<34} {}",
                    Local::now().format("%H:%M:%S"),
                    e.kind,
                    e.label.chars().take(34).collect::<String>(),
                    naam(&e.actor)
                );
            }
            Err(msg) => {
                // The rest of this case depends on this event: drop it.
                cases[i].items.clear();
                failed += 1;
                eprintln!("FOUT {} ({}): {msg}", e.kind, e.label);
                if failed > 20 && failed > sent {
                    eprintln!("Te veel fouten, gestopt.");
                    std::process::exit(1);
                }
            }
        }
        if cases[i].items.is_empty() {
            cases.swap_remove(i);
        }
        if rate > 0.0 {
            let pause = w.rng.gen_range(0.3..1.7) / rate;
            std::thread::sleep(Duration::from_secs_f64(pause));
        }
    }
    eprintln!("klaar: {sent} events, {failed} fouten");
}
