//! openevents-sim: sends a stream of semi-realistic events to an OpenEvents in dev mode
//! (OPENEVENTS_DEV_AUTH=true), using the example profiles `nl.vng.meldingen`,
//! `nl.vng.vergunningen`, `nl.vng.gesprekken` and `nl.vng.taken`.
//!
//! Every zaak is a scenario: a list of steps with realistic gaps between them. Meldingen
//! come in via a meldingen-API (Signalen) or the KCC, aanvragen via the Omgevingsloket,
//! and gesprekken are linked to their zaak via `aanleidingZaak`. Every actor posts with
//! its own dev token, so the read and write rules of the profiles apply exactly as for
//! real clients.
//!
//! With `--backfill N` it first writes N zaken as history (event times in the past, steps
//! that would lie in the future are left out so recent zaken stay open). Then it plays
//! zaken live, interleaved, until `--count` is reached.

use chrono::{DateTime, Duration as Mins, Local, Utc};
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::time::Duration;

const ID: &str = "https://id.example.nl/";
const G: &str = "nl.vng.gesprekken";
const M: &str = "nl.vng.meldingen.melding";
const V: &str = "nl.vng.vergunningen.aanvraag";
const TK: &str = "nl.vng.taken.taak";

// ---------- actors ----------

#[derive(Clone)]
struct Actor {
    id: String,
    slug: String,
    claims: Vec<(&'static str, &'static str)>,
    source: &'static str,
}

impl Actor {
    fn new(slug: &str, claims: &[(&'static str, &'static str)]) -> Actor {
        let rol = claims
            .iter()
            .find(|(k, _)| *k == "rol")
            .map(|(_, v)| *v)
            .unwrap_or("");
        let source = match (slug, rol) {
            ("signalen", _) => "https://signalen.demodam.nl",
            ("omgevingsloket", _) => "https://omgevingsloket.nl",
            (_, "kcc") => "https://kcc.demodam.nl",
            (_, "medewerker") => "https://zaaksysteem.demodam.nl",
            _ => "https://mijn.demodam.nl",
        };
        Actor {
            id: format!("{ID}{slug}"),
            slug: slug.to_string(),
            claims: claims.to_vec(),
            source,
        }
    }
    /// With OPENEVENTS_JWT_SECRET: an HS256 token signed with the secret the server
    /// shares (works when dev tokens are read-only). `sub` is the full id and
    /// `preferred_username` the short name, so both actor-claim settings give the
    /// same actor. Without it: a dev token.
    fn token(&self) -> String {
        if let Some(secret) = std::env::var("OPENEVENTS_JWT_SECRET")
            .ok()
            .filter(|s| !s.is_empty())
        {
            let mut claims = serde_json::Map::new();
            claims.insert("sub".into(), json!(self.id));
            claims.insert("preferred_username".into(), json!(self.slug));
            claims.insert("exp".into(), json!(Utc::now().timestamp() + 3600));
            for (k, v) in &self.claims {
                let vals: Vec<&str> = v.split(',').collect();
                claims.insert(
                    (*k).into(),
                    if vals.len() == 1 {
                        json!(v)
                    } else {
                        json!(vals)
                    },
                );
            }
            return jsonwebtoken::encode(
                &jsonwebtoken::Header::default(),
                &claims,
                &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
            )
            .expect("sign token");
        }
        let mut t = format!("dev:{}", self.id);
        for (k, v) in &self.claims {
            t.push_str(&format!("|{k}={v}"));
        }
        t
    }
}

// ---------- scenarios ----------

struct Ev {
    /// Minutes after the previous step of the same zaak (used for history).
    after: i64,
    actor: Actor,
    kind: String,
    subject: String,
    label: String,
    data: Value,
}

/// A zaak and its gesprekken as a list of steps.
struct Seq {
    start: DateTime<Utc>,
    t: i64,
    steps: VecDeque<Ev>,
    /// The deelnames per gesprek so far.
    deelnames: Vec<(String, Vec<Value>)>,
}

impl Seq {
    fn new(start: DateTime<Utc>) -> Seq {
        Seq {
            start,
            t: 0,
            steps: VecDeque::new(),
            deelnames: Vec::new(),
        }
    }
    fn at(
        &mut self,
        after: i64,
        actor: &Actor,
        kind: &str,
        subject: &str,
        label: &str,
        data: Value,
    ) {
        self.t += after;
        self.steps.push_back(Ev {
            after,
            actor: actor.clone(),
            kind: kind.to_string(),
            subject: subject.to_string(),
            label: label.to_string(),
            data,
        });
    }
    /// A new resource, created by an action with one create.
    fn create(
        &mut self,
        after: i64,
        actor: &Actor,
        kind: &str,
        subject: &str,
        label: &str,
        data: Value,
    ) {
        self.at(
            after,
            actor,
            kind,
            subject,
            label,
            json!({ "resourceData": data }),
        );
    }
    /// A new resource with a fresh id (a bijdrage, aanvulling, advies); returns the id.
    fn create_new(
        &mut self,
        after: i64,
        actor: &Actor,
        kind: &str,
        label: &str,
        data: Value,
    ) -> String {
        let id = format!("urn:uuid:{}", uuid::Uuid::now_v7());
        self.create(after, actor, kind, &id, label, data);
        id
    }
    /// What changed, as a JSON Merge Patch.
    fn patch(
        &mut self,
        after: i64,
        actor: &Actor,
        kind: &str,
        subject: &str,
        label: &str,
        data: Value,
    ) {
        self.at(after, actor, kind, subject, label, json!({ "patch": data }));
    }
    /// Start a gesprek; `initiatiefnemer` becomes the first deelname. The scenarios
    /// write `onderwerp` and `aanleiding.zaak`; here they get the names from the lab
    /// schema (`gespreksonderwerp`, `aanleidingZaak`).
    fn gesprek(&mut self, after: i64, actor: &Actor, g: &str, label: &str, data: Value) {
        let first = deelname_van(&data["initiatiefnemer"]);
        self.deelnames.push((g.to_string(), vec![first.clone()]));
        let zaak = data["aanleiding"]["zaak"].clone();
        let mut data = json!({
            "gespreksonderwerp": data["onderwerp"],
            "team": data["team"],
            "gespreksdeelnames": [first],
        });
        if zaak.is_string() {
            data["aanleidingZaak"] = zaak;
        }
        self.at(
            after,
            actor,
            &format!("{G}.gesprek.gestart"),
            g,
            label,
            json!({ "type": format!("{G}.gesprek"), "resourceData": data }),
        );
    }
    /// A bijdrage is its own resource, pointing at its gesprek.
    fn bijdrage(&mut self, after: i64, actor: &Actor, g: &str, label: &str, mut data: Value) {
        data["gesprek"] = json!(g);
        if let Some(t) = data.as_object_mut().and_then(|o| o.remove("tekst")) {
            data["tekstGespreksBijdrage"] = t;
        }
        self.create_new(
            after,
            actor,
            &format!("{G}.bijdrage.toegevoegd"),
            label,
            data,
        );
    }
    /// Deelnames are a small list: the client sends all of it.
    fn deelname(&mut self, after: i64, actor: &Actor, g: &str, label: &str, d: Value) {
        let d = deelname_van(&d);
        let list = match self.deelnames.iter_mut().find(|(id, _)| id == g) {
            Some((_, l)) => {
                l.push(d);
                l.clone()
            }
            None => vec![d],
        };
        self.patch(
            after,
            actor,
            &format!("{G}.deelname.toegevoegd"),
            g,
            label,
            json!({ "gespreksdeelnames": list }),
        );
    }
    /// A date `days` after a step that happens `after` minutes from now in this zaak.
    fn date(&self, after: i64, days: i64) -> String {
        (self.start + Mins::minutes(self.t + after) + Mins::days(days))
            .date_naive()
            .to_string()
    }
}

struct MeldingSoort {
    categorie: &'static str,
    sub: &'static str,
    team: &'static str,
    teksten: &'static [&'static str],
    vraag: &'static str,
    uitvoerder: &'static str,
}

const MELDINGEN: &[MeldingSoort] = &[
    MeldingSoort {
        categorie: "Wegen, verkeer, straatmeubilair",
        sub: "Losse stoeptegel",
        team: "buitenruimte",
        teksten: &[
            "Er ligt een stoeptegel los voor nummer {nr}, mijn buurvrouw is er bijna over gestruikeld.",
            "Meerdere tegels liggen scheef en los, gevaarlijk voor mensen met een rollator.",
        ],
        vraag: "Kunt u ongeveer zeggen wanneer dit wordt gemaakt? Er lopen hier veel ouderen.",
        uitvoerder: "Stratenmakersbedrijf Van Dijk",
    },
    MeldingSoort {
        categorie: "Wegen, verkeer, straatmeubilair",
        sub: "Gat in de weg",
        team: "buitenruimte",
        teksten: &[
            "Diep gat in het wegdek, fietsers moeten uitwijken naar de rijbaan.",
            "Na de vorst is er een flink gat in de weg ontstaan bij de kruising.",
        ],
        vraag: "Is het mogelijk om alvast een bord of afzetting te plaatsen?",
        uitvoerder: "Wegenonderhoud BV",
    },
    MeldingSoort {
        categorie: "Straatverlichting",
        sub: "Lantaarnpaal kapot",
        team: "buitenruimte",
        teksten: &[
            "De lantaarnpaal bij het speeltuintje brandt al een week niet.",
            "Lantaarnpaal knippert de hele nacht aan en uit.",
        ],
        vraag: "Het is 's avonds echt donker in de straat, kan het voor het weekend?",
        uitvoerder: "de netbeheerder",
    },
    MeldingSoort {
        categorie: "Groen en water",
        sub: "Boom of tak gevallen",
        team: "buitenruimte",
        teksten: &[
            "Na de storm ligt er een grote tak over het fietspad.",
            "Er hangt een afgebroken tak boven de stoep, die kan elk moment vallen.",
        ],
        vraag: "Moet ik de tak zelf aan de kant leggen of komt er iemand?",
        uitvoerder: "Hoveniersbedrijf De Groene Hand",
    },
    MeldingSoort {
        categorie: "Groen en water",
        sub: "Verstopte put",
        team: "buitenruimte",
        teksten: &["Bij regen staat de hele straat blank, de putten zijn verstopt met bladeren."],
        vraag: "Het water komt bijna tot aan onze voordeur, kan dit met spoed?",
        uitvoerder: "Rioolservice West",
    },
    MeldingSoort {
        categorie: "Afval",
        sub: "Grofvuil naast container",
        team: "afval",
        teksten: &[
            "Er staat een bank en een matras naast de ondergrondse container.",
            "Iemand heeft vuilniszakken naast de container gezet, de meeuwen trekken ze open.",
        ],
        vraag: "Wordt de vervuiler hier ook op aangesproken?",
        uitvoerder: "Afvalinzameling Demodam",
    },
    MeldingSoort {
        categorie: "Afval",
        sub: "Container vol of kapot",
        team: "afval",
        teksten: &[
            "De papiercontainer zit vol en de klep klemt.",
            "De pasjeslezer van de restafvalcontainer werkt niet.",
        ],
        vraag: "Waar kan ik mijn afval in de tussentijd kwijt?",
        uitvoerder: "Afvalinzameling Demodam",
    },
    MeldingSoort {
        categorie: "Overlast",
        sub: "Geluidsoverlast horeca",
        team: "handhaving",
        teksten: &[
            "Het café op de hoek draait na 01:00 nog harde muziek met de deuren open.",
            "Elke vrijdagnacht veel lawaai van bezoekers die buiten blijven hangen.",
        ],
        vraag: "Dit gebeurt elke week. Wat kan de gemeente hier structureel aan doen?",
        uitvoerder: "het boa-team Centrum",
    },
    MeldingSoort {
        categorie: "Overlast",
        sub: "Fout geparkeerde fietsen",
        team: "handhaving",
        teksten: &["Bij de ingang van het station staan tientallen fietsen op de geleidelijn voor blinden."],
        vraag: "Worden de fietsen weggehaald of krijgen mensen eerst een waarschuwing?",
        uitvoerder: "het fietsdepot",
    },
];

struct AanvraagSoort {
    activiteiten: &'static [&'static str],
    omschrijving: &'static str,
    bijlagen: &'static [&'static str],
    adviseur: &'static str,
    vraag: &'static str,
    aanvulling: &'static [&'static str],
    kosten: (u32, u32),
    uitgebreid: bool,
}

const AANVRAGEN: &[AanvraagSoort] = &[
    AanvraagSoort {
        activiteiten: &["bouwen"],
        omschrijving: "Plaatsen van een dakkapel aan de achterzijde, breedte 3,2 meter.",
        bijlagen: &[
            "bouwtekening-dakkapel.pdf",
            "constructieberekening.pdf",
            "foto-bestaande-situatie.jpg",
        ],
        adviseur: "Welstandscommissie",
        vraag: "Past de dakkapel binnen de welstandsnota?",
        aanvulling: &["constructieberekening met handtekening constructeur"],
        kosten: (8000, 18000),
        uitgebreid: false,
    },
    AanvraagSoort {
        activiteiten: &["kappen"],
        omschrijving:
            "Kappen van een eik in de voortuin vanwege wortelopdruk en schade aan de fundering.",
        bijlagen: &["foto-boom.jpg", "situatieschets.pdf"],
        adviseur: "Bomenconsulent",
        vraag: "Is de boom waardevol en is herplant mogelijk?",
        aanvulling: &["rapport boomveiligheidscontrole"],
        kosten: (0, 0),
        uitgebreid: false,
    },
    AanvraagSoort {
        activiteiten: &["bouwen"],
        omschrijving:
            "Uitbreiding van de woonkamer met een aanbouw van 4 bij 3 meter aan de achtergevel.",
        bijlagen: &[
            "plattegrond.pdf",
            "doorsnede.pdf",
            "gevelaanzichten.pdf",
            "BENG-berekening.pdf",
        ],
        adviseur: "Welstandscommissie",
        vraag: "Graag advies over materiaalgebruik en kleur.",
        aanvulling: &["daglichtberekening", "toelichting afvoer hemelwater"],
        kosten: (25000, 60000),
        uitgebreid: false,
    },
    AanvraagSoort {
        activiteiten: &["uitrit"],
        omschrijving: "Aanleggen van een inrit van 3 meter breed naar de oprit.",
        bijlagen: &["situatietekening-uitweg.pdf"],
        adviseur: "Verkeerskundige",
        vraag: "Is de uitweg verkeersveilig gezien de nabijgelegen school?",
        aanvulling: &["maatvoering ten opzichte van de lantaarnpaal"],
        kosten: (2000, 5000),
        uitgebreid: false,
    },
    AanvraagSoort {
        activiteiten: &["bouwen", "monument"],
        omschrijving:
            "Vervangen van kozijnen in de voorgevel door houten kozijnen met isolatieglas.",
        bijlagen: &[
            "bouwhistorische-verkenning.pdf",
            "detailtekeningen-kozijnen.pdf",
            "kleurenschema.pdf",
        ],
        adviseur: "Erfgoedcommissie",
        vraag: "Graag advies over de profielen en het glas.",
        aanvulling: &["detail aansluiting kozijn op metselwerk"],
        kosten: (15000, 40000),
        uitgebreid: true,
    },
    AanvraagSoort {
        activiteiten: &["monument"],
        omschrijving: "Plaatsen van 14 zonnepanelen op het achterdakvlak.",
        bijlagen: &["legplan.pdf", "foto-dakvlak.jpg"],
        adviseur: "Erfgoedcommissie",
        vraag: "Zijn de panelen vanaf de openbare weg zichtbaar?",
        aanvulling: &["zichtlijnenstudie vanaf de straat"],
        kosten: (6000, 12000),
        uitgebreid: false,
    },
];

const STRATEN: &[(&str, &str)] = &[
    ("Kerkstraat", "Centrum"),
    ("Molenweg", "Noord"),
    ("Lindenlaan", "Oost"),
    ("Havenkade", "Centrum"),
    ("Beatrixstraat", "West"),
    ("Populierenhof", "Zuid"),
    ("Dorpsstraat", "Oud-Zuid"),
    ("Schoolstraat", "Noord"),
    ("Merelstraat", "West"),
    ("Industrieweg", "Bedrijventerrein De Kamp"),
    ("Julianaplein", "Centrum"),
    ("Esdoornlaan", "Oost"),
    ("Weidepad", "Zuid"),
    ("Stationsweg", "Centrum"),
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

/// A deelname as in the lab schema: a medewerker is an actor, everyone else a partij.
fn deelname_van(d: &Value) -> Value {
    let id = d["partij"].as_str().unwrap_or_default();
    let slug = id.rsplit('/').next().unwrap_or(id);
    let mut c = slug.chars();
    let naam = c
        .next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default();
    if d["rol"] == "behandelaar" {
        json!({ "type": "actor", "actor": id, "naam": naam })
    } else {
        json!({ "type": "partij", "partij": id, "naam": naam })
    }
}

struct World {
    rng: StdRng,
    inwoners: Vec<Actor>,
    personas: Vec<Actor>,
    teams: Vec<(&'static str, Vec<Actor>)>,
    kcc: Vec<Actor>,
    signalen: Actor,
    olo: Actor,
    gemachtigde: Actor,
}

impl World {
    fn new(seed: u64) -> World {
        let inw = |n: &str| Actor::new(n, &[("rol", "inwoner")]);
        let team = |name: &'static str, names: &[&str]| {
            let list = names
                .iter()
                .map(|n| Actor::new(n, &[("rol", "medewerker"), ("team", name)]))
                .collect();
            (name, list)
        };
        World {
            rng: StdRng::seed_from_u64(seed),
            personas: ["priya", "jan", "suzanne", "karel"]
                .iter()
                .map(|n| inw(n))
                .collect(),
            inwoners: [
                "fatima", "henk", "lotte", "mohammed", "ingrid", "tim", "yara", "kees", "esther",
                "daan", "sevda", "pieter", "anouk", "hamza", "ilse", "zeynep",
            ]
            .iter()
            .map(|n| inw(n))
            .collect(),
            // The first of each team is a persona and gets half of the work.
            teams: vec![
                team("buitenruimte", &["ahmed", "sanne", "dirk"]),
                team("afval", &["wouter", "petra"]),
                team("handhaving", &["bram", "noor"]),
                team("vergunningen", &["femke", "rob"]),
                team("wmo", &["merve", "ellen"]),
            ],
            kcc: vec![
                Actor::new("kim", &[("rol", "kcc")]),
                Actor::new("tom", &[("rol", "kcc")]),
            ],
            signalen: Actor::new("signalen", &[("rol", "systeem")]),
            olo: Actor::new("omgevingsloket", &[("rol", "systeem")]),
            gemachtigde: Actor::new("architectenbureau-de-boog", &[("rol", "gemachtigde")]),
        }
    }

    fn pick<T: Copy>(&mut self, list: &[T]) -> T {
        *list.choose(&mut self.rng).unwrap()
    }
    fn kans(&mut self, p: f64) -> bool {
        self.rng.gen_bool(p)
    }
    fn min(&mut self, lo: i64, hi: i64) -> i64 {
        self.rng.gen_range(lo..=hi)
    }

    fn inwoner(&mut self) -> Actor {
        if self.kans(0.4) {
            self.personas.choose(&mut self.rng).unwrap().clone()
        } else {
            self.inwoners.choose(&mut self.rng).unwrap().clone()
        }
    }

    fn medewerker(&mut self, team: &str) -> Actor {
        let list = self
            .teams
            .iter()
            .find(|(t, _)| *t == team)
            .unwrap()
            .1
            .clone();
        if self.kans(0.5) {
            list[0].clone()
        } else {
            list.choose(&mut self.rng).unwrap().clone()
        }
    }

    fn kcc_medewerker(&mut self) -> Actor {
        self.kcc.choose(&mut self.rng).unwrap().clone()
    }

    fn nummer(&mut self, prefix: &str, start: DateTime<Utc>) -> String {
        format!(
            "{prefix}-{}-{:06}",
            start.format("%Y"),
            self.rng.gen_range(100_000..999_999)
        )
    }

    fn plek(&mut self) -> (String, String, Value) {
        let (straat, wijk) = self.pick(STRATEN);
        let nr = self.rng.gen_range(1..180);
        let r = |x: f64| (x * 1e5).round() / 1e5;
        let lon = r(5.10 + self.rng.gen::<f64>() * 0.05);
        let lat = r(52.08 + self.rng.gen::<f64>() * 0.04);
        (
            format!("{straat} {nr}, Demodam"),
            wijk.to_string(),
            json!({ "type": "Point", "coordinates": [lon, lat] }),
        )
    }

    fn scenario(&mut self, start: DateTime<Utc>) -> Seq {
        match self.rng.gen_range(0..100) {
            0..=59 => self.melding(start),
            60..=89 => self.vergunning(start),
            _ => self.wmo_gesprek(start),
        }
    }

    fn melding(&mut self, start: DateTime<Utc>) -> Seq {
        let m = &MELDINGEN[self.rng.gen_range(0..MELDINGEN.len())];
        let inwoner = self.inwoner();
        let mw = self.medewerker(m.team);
        let nr = self.nummer("MOR", start);
        let zaak = format!("https://zaken.demodam.nl/meldingen/{nr}");
        let (adres, wijk, locatie) = self.plek();
        let huisnr = self.rng.gen_range(1..120).to_string();
        let tekst = self.pick(m.teksten).replace("{nr}", &huisnr);
        let (indiener, kanaal) = if self.kans(0.2) {
            (self.kcc_medewerker(), "telefoon")
        } else {
            (
                self.signalen.clone(),
                self.pick(&["app", "app", "website", "e-mail"]),
            )
        };
        let prioriteit = self.pick(&["normaal", "normaal", "laag", "hoog"]);
        let fotos = self.pick(&[0, 1, 1, 2, 3]);
        let mut s = Seq::new(start);
        s.create(
            0,
            &indiener,
            &format!("{M}.gedaan"),
            &zaak,
            &nr,
            json!({
                "zaaknummer": nr, "categorie": m.categorie, "subcategorie": m.sub,
                "omschrijving": tekst, "locatie": locatie, "adres": adres, "wijk": wijk,
                "kanaal": kanaal, "team": m.team, "prioriteit": prioriteit, "fotos": fotos,
                "melder": { "partij": inwoner.id, "rol": "inwoner" }
            }),
        );
        let a = self.min(20, 900);
        s.patch(
            a,
            &mw,
            &format!("{M}.toegewezen"),
            &zaak,
            &nr,
            json!({ "behandelaar": mw.id }),
        );

        // A gesprek next to the melding: the inwoner asks, or the medewerker needs more.
        let mut gesprek: Option<(String, String)> = None;
        let r: f64 = self.rng.gen();
        if r < 0.35 {
            let g = format!("urn:uuid:{}", uuid::Uuid::now_v7());
            let onderwerp = format!("Vraag over melding {nr}");
            let antwoord = self.pick(&[
                "We hebben het ingepland, de verwachting is binnen 5 werkdagen.",
                "Goed dat u het meldt. Een collega gaat vandaag of morgen kijken.",
                "We hebben een tijdelijke afzetting geplaatst, herstel volgt deze week.",
            ]);
            let a = self.min(30, 600);
            s.gesprek(
                a,
                &inwoner,
                &g,
                &onderwerp,
                json!({
                    "onderwerp": onderwerp, "team": m.team, "aanleiding": { "zaak": zaak },
                    "initiatiefnemer": { "partij": inwoner.id, "rol": "inwoner" }
                }),
            );
            s.bijdrage(1, &inwoner, &g, &onderwerp, json!({ "tekst": m.vraag }));
            let a = self.min(30, 400);
            s.deelname(
                a,
                &mw,
                &g,
                &onderwerp,
                json!({ "partij": mw.id, "rol": "behandelaar" }),
            );
            s.bijdrage(
                2,
                &mw,
                &g,
                &onderwerp,
                json!({ "tekst": antwoord, "automatisch": false }),
            );
            gesprek = Some((g, onderwerp));
        } else if r < 0.6 {
            let g = format!("urn:uuid:{}", uuid::Uuid::now_v7());
            let onderwerp = format!("Aanvullende vraag bij melding {nr}");
            let straat = adres.split(',').next().unwrap_or(&adres).to_string();
            let vraag = self
                .pick(&[
                    "Wij kunnen de locatie bij {s} niet goed vinden. Kunt u een foto sturen?",
                    "Is het nog steeds zo, of is het inmiddels opgelost?",
                    "Klopt het dat het om de kant van de school gaat?",
                ])
                .replace("{s}", &straat);
            let antwoord = self.pick(&[
                "Ja, het is er nog steeds. Ik heb net een foto bij de melding gezet.",
                "Aan de kant van de school inderdaad, vlak bij het zebrapad.",
                "Het is zelfs erger geworden sinds gisteren.",
            ]);
            let a = self.min(10, 240);
            s.gesprek(
                a,
                &mw,
                &g,
                &onderwerp,
                json!({
                    "onderwerp": onderwerp, "team": m.team, "aanleiding": { "zaak": zaak },
                    "initiatiefnemer": { "partij": mw.id, "rol": "behandelaar" }
                }),
            );
            s.deelname(
                1,
                &mw,
                &g,
                &onderwerp,
                json!({ "partij": inwoner.id, "rol": "inwoner" }),
            );
            s.bijdrage(
                1,
                &mw,
                &g,
                &onderwerp,
                json!({ "tekst": vraag, "automatisch": false }),
            );
            let a = self.min(20, 400);
            s.bijdrage(a, &inwoner, &g, &onderwerp, json!({ "tekst": antwoord }));
            s.create_new(2, &inwoner, &format!("{M}.aangevuld"), &nr, json!({ "melding": zaak, "tekst": "Extra foto toegevoegd via het gesprek.", "fotos": 1 }));
            gesprek = Some((g, onderwerp));
        } else if r < 0.75 {
            let tekst = self.pick(&[
                "Het wordt steeds erger.",
                "Vanochtend is er bijna een fietser gevallen.",
                "Inmiddels hebben meer buren hier last van.",
            ]);
            let a = self.min(60, 1440);
            s.create_new(
                a,
                &inwoner,
                &format!("{M}.aangevuld"),
                &nr,
                json!({ "melding": zaak, "tekst": tekst }),
            );
        }
        if self.kans(0.6) {
            let a = self.min(120, 2880);
            let dagen = self.rng.gen_range(1..10);
            let datum = s.date(a, dagen);
            s.patch(a, &mw, &format!("{M}.ingepland"), &zaak, &nr, json!({ "ingepland": json!({ "datum": datum, "uitvoerder": m.uitvoerder, "toelichting": "Werkzaamheden ingepland." }) }));
        }
        let resultaat = match self.rng.gen_range(0..11) {
            0..=7 => "opgelost",
            8..=9 => "doorgezet naar derde",
            _ => "dubbele melding",
        };
        let toelichting = match resultaat {
            "opgelost" => format!("Het probleem is verholpen door {}.", m.uitvoerder),
            "doorgezet naar derde" => {
                format!("Doorgezet naar {}, die neemt het verder op.", m.uitvoerder)
            }
            _ => "Er was al een melding over deze locatie, die is inmiddels afgehandeld.".into(),
        };
        let a = self.min(240, 4320);
        s.patch(
            a,
            &mw,
            &format!("{M}.afgehandeld"),
            &zaak,
            &nr,
            json!({ "resultaat": resultaat, "toelichting": toelichting }),
        );
        if let Some((g, onderwerp)) = &gesprek {
            let a = self.min(5, 120);
            s.bijdrage(a, &mw, g, onderwerp, json!({
                    "tekst": "De melding is afgehandeld. Laat het ons weten als het probleem terugkomt.",
                    "automatisch": false
                }));
            s.patch(
                1,
                &mw,
                &format!("{G}.gesprek.afgesloten"),
                g,
                onderwerp,
                json!({ "afsluitreden": "Melding afgehandeld" }),
            );
        }
        if self.kans(0.08) {
            let a = self.min(600, 4320);
            s.patch(
                a,
                &inwoner,
                &format!("{M}.heropend"),
                &zaak,
                &nr,
                json!({ "heropenreden": "Het probleem is er nog steeds." }),
            );
            let a = self.min(240, 2880);
            s.patch(a, &mw, &format!("{M}.afgehandeld"), &zaak, &nr, json!({ "resultaat": "opgelost", "toelichting": "Opnieuw bekeken en nu echt opgelost." }));
        }
        if self.kans(0.4) {
            let score = self.pick(&[5, 5, 5, 5, 5, 4, 4, 4, 4, 3, 3, 2, 1]);
            let opmerking = self.pick(&[
                "Snel opgelost, dank!",
                "Duurde wat lang.",
                "Prima geholpen.",
                "",
            ]);
            let a = self.min(60, 2880);
            let signalen = self.signalen.clone();
            s.patch(
                a,
                &signalen,
                &format!("{M}.tevredenheid_gegeven"),
                &zaak,
                &nr,
                json!({ "tevredenheid": json!({ "score": score, "opmerking": opmerking }) }),
            );
        }
        s
    }

    fn vergunning(&mut self, start: DateTime<Utc>) -> Seq {
        let v = &AANVRAGEN[self.rng.gen_range(0..AANVRAGEN.len())];
        let aanvrager = self.inwoner();
        let mw = self.medewerker("vergunningen");
        let olo = self.olo.clone();
        let nr = self.nummer("OV", start);
        let zaak = format!("https://zaken.demodam.nl/vergunningen/{nr}");
        let (adres, wijk, locatie) = self.plek();
        let mut s = Seq::new(start);
        let mut data = json!({
            "zaaknummer": nr,
            "verzoeknummer": self.rng.gen_range(9_000_000..9_999_999).to_string(),
            "activiteiten": v.activiteiten, "omschrijving": v.omschrijving,
            "procedure": if v.uitgebreid { "uitgebreid" } else { "regulier" },
            "locatie": locatie, "adres": adres, "wijk": wijk,
            "kadastraal": format!("DMD00 K {}", self.rng.gen_range(1000..9999)),
            "bijlagen": v.bijlagen,
            "aanvrager": { "partij": aanvrager.id, "rol": "aanvrager" }
        });
        if v.kosten.1 > 0 {
            data["bouwkosten"] = json!(self.rng.gen_range(v.kosten.0..v.kosten.1) / 100 * 100);
        }
        // With a gemachtigde (an architect), the architect sends the aanvulling.
        let indiener_aanvulling = if self.kans(0.3) {
            data["gemachtigde"] = json!(self.gemachtigde.id);
            self.gemachtigde.clone()
        } else {
            aanvrager.clone()
        };
        s.create(0, &olo, &format!("{V}.ingediend"), &zaak, &nr, data);
        let a = self.min(600, 4320);
        let beslis = s.date(a, if v.uitgebreid { 26 * 7 } else { 8 * 7 });
        s.patch(
            a,
            &mw,
            &format!("{V}.ontvankelijk_verklaard"),
            &zaak,
            &nr,
            json!({ "behandelaar": mw.id, "uiterlijkeBeslisdatum": beslis }),
        );
        if self.kans(0.45) {
            let a = self.min(600, 2880);
            let termijn = s.date(a, 14);
            s.patch(
                a,
                &mw,
                &format!("{V}.aanvulling_gevraagd"),
                &zaak,
                &nr,
                json!({ "aanvullingGevraagd": json!({
                    "gevraagd": v.aanvulling, "termijnTot": termijn,
                    "toelichting": "De beslistermijn is opgeschort tot de stukken binnen zijn."
                }) }),
            );
            // A taak for the aanvrager in the inwoner's overview (MijnTaken).
            let taak = s.create_new(
                1,
                &mw,
                &format!("{TK}.aangemaakt"),
                &nr,
                json!({
                    "titel": format!("Lever aanvullende stukken aan voor {nr}"),
                    "toelichting": format!("Nodig: {}.", v.aanvulling.join(", ")),
                    "zaak": zaak, "voor": { "partij": aanvrager.id }, "team": "vergunningen",
                    "deadline": termijn
                }),
            );
            let g = format!("urn:uuid:{}", uuid::Uuid::now_v7());
            let onderwerp = format!("Aanvulling nodig voor {nr}");
            s.gesprek(
                5,
                &mw,
                &g,
                &onderwerp,
                json!({
                    "onderwerp": onderwerp, "team": "vergunningen", "aanleiding": { "zaak": zaak },
                    "initiatiefnemer": { "partij": mw.id, "rol": "behandelaar" }
                }),
            );
            s.deelname(
                1,
                &mw,
                &g,
                &onderwerp,
                json!({ "partij": aanvrager.id, "rol": "inwoner" }),
            );
            let tekst = format!(
                "Voor uw aanvraag missen we nog: {}. U kunt dit aanleveren tot {termijn}.",
                v.aanvulling.join(", ")
            );
            s.bijdrage(
                1,
                &mw,
                &g,
                &onderwerp,
                json!({ "tekst": tekst, "automatisch": false }),
            );
            let vraag = self.pick(&[
                "Dank voor het bericht. Mijn aannemer stuurt het deze week op.",
                "Moet dat document ook ondertekend zijn?",
                "Ik begrijp niet goed wat u precies nodig heeft, kunt u bellen?",
            ]);
            let a = self.min(60, 1440);
            s.bijdrage(a, &aanvrager, &g, &onderwerp, json!({ "tekst": vraag }));
            let antwoord = self.pick(&[
                "Ja, graag ondertekend door de constructeur.",
                "Prima, u kunt het uploaden via het Omgevingsloket.",
                "Ik bel u morgenochtend.",
            ]);
            let a = self.min(30, 480);
            s.bijdrage(
                a,
                &mw,
                &g,
                &onderwerp,
                json!({ "tekst": antwoord, "automatisch": false }),
            );
            let bijlagen: Vec<String> = v
                .aanvulling
                .iter()
                .map(|x| format!("{}.pdf", x.replace(' ', "-")))
                .collect();
            let a = self.min(1440, 7200);
            s.patch(a, &indiener_aanvulling, &format!("{V}.aanvulling_ontvangen"), &zaak, &nr, json!({ "aanvullingOntvangen": json!({ "bijlagen": bijlagen, "toelichting": "Zie bijlage." }) }));
            s.patch(2, &mw, &format!("{TK}.afgerond"), &taak, &nr, json!({}));
            let a = self.min(10, 600);
            s.patch(
                a,
                &mw,
                &format!("{G}.gesprek.afgesloten"),
                &g,
                &onderwerp,
                json!({ "afsluitreden": "Aanvulling ontvangen" }),
            );
        }
        if self.kans(0.08) {
            let a = self.min(1440, 10080);
            s.patch(a, &olo, &format!("{V}.ingetrokken"), &zaak, &nr, json!({ "intrekkingsreden": "Aanvrager heeft de aanvraag ingetrokken via het Omgevingsloket." }));
            return s;
        }
        if v.uitgebreid || self.kans(0.6) {
            let a = self.min(600, 2880);
            let advies = s.create_new(
                a,
                &mw,
                &format!("{V}.advies_gevraagd"),
                &nr,
                json!({ "aanvraag": zaak, "adviseur": v.adviseur, "vraag": v.vraag }),
            );
            let uitkomst = self.pick(&[
                "positief",
                "positief",
                "positief",
                "positief met voorwaarden",
                "positief met voorwaarden",
                "negatief",
            ]);
            let toelichting = match uitkomst {
                "positief" => "Past binnen het beleid.",
                "negatief" => "Niet passend in het straatbeeld.",
                _ => "Akkoord, mits de kleur wordt aangepast.",
            };
            let a = self.min(4320, 20160);
            s.patch(
                a,
                &mw,
                &format!("{V}.advies_ontvangen"),
                &advies,
                &nr,
                json!({ "uitkomst": uitkomst, "toelichting": toelichting }),
            );
        }
        if self.kans(0.15) {
            let a = self.min(600, 2880);
            let nieuw = s.date(a, 14 * 7);
            s.patch(a, &mw, &format!("{V}.termijn_verlengd"), &zaak, &nr, json!({ "uiterlijkeBeslisdatum": nieuw, "verlengingsreden": "Advies van een externe partij duurt langer." }));
        }
        let uitkomst = match self.rng.gen_range(0..100) {
            0..=54 => "verleend",
            55..=84 => "verleend met voorschriften",
            85..=94 => "geweigerd",
            _ => "buiten behandeling",
        };
        let a = self.min(1440, 10080);
        let mut besluit = json!({ "uitkomst": uitkomst, "bezwaartermijnTot": s.date(a, 42) });
        if uitkomst == "verleend met voorschriften" {
            let all = [
                "Start bouw melden 2 dagen vooraf",
                "Herplantplicht binnen 12 maanden",
                "Kleur kozijnen RAL 9001",
                "Geen werkzaamheden in het broedseizoen",
            ];
            let gekozen: Vec<&str> = all.choose_multiple(&mut self.rng, 2).copied().collect();
            besluit["voorschriften"] = json!(gekozen);
        }
        s.patch(
            a,
            &mw,
            &format!("{V}.besloten"),
            &zaak,
            &nr,
            json!({ "besluit": besluit }),
        );
        if uitkomst.starts_with("verleend") {
            let publicatie = format!(
                "https://zoek.officielebekendmakingen.nl/gmb-{}-{}",
                start.format("%Y"),
                self.rng.gen_range(100_000..999_999)
            );
            let a = self.min(60, 2880);
            s.patch(
                a,
                &mw,
                &format!("{V}.bekendgemaakt"),
                &zaak,
                &nr,
                json!({ "bekendmaking": json!({ "publicatie": publicatie }) }),
            );
        }
        s
    }

    fn wmo_gesprek(&mut self, start: DateTime<Utc>) -> Seq {
        let inwoner = self.inwoner();
        let mw = self.medewerker("wmo");
        let (onderwerp, vraag, antwoord) = self.pick(WMO);
        let g = format!("urn:uuid:{}", uuid::Uuid::now_v7());
        let mut s = Seq::new(start);
        s.gesprek(
            0,
            &inwoner,
            &g,
            onderwerp,
            json!({
                "onderwerp": onderwerp, "team": "wmo",
                "initiatiefnemer": { "partij": inwoner.id, "rol": "inwoner" }
            }),
        );
        s.bijdrage(1, &inwoner, &g, onderwerp, json!({ "tekst": vraag }));
        let a = self.min(30, 600);
        s.deelname(
            a,
            &mw,
            &g,
            onderwerp,
            json!({ "partij": mw.id, "rol": "behandelaar" }),
        );
        s.bijdrage(
            2,
            &mw,
            &g,
            onderwerp,
            json!({ "tekst": antwoord, "automatisch": false }),
        );
        let a = self.min(10, 600);
        s.bijdrage(
            a,
            &inwoner,
            &g,
            onderwerp,
            json!({ "tekst": "Dank u wel." }),
        );
        let a = self.min(10, 1440);
        s.patch(
            a,
            &mw,
            &format!("{G}.gesprek.afgesloten"),
            &g,
            onderwerp,
            json!({ "afsluitreden": "Vraag beantwoord" }),
        );
        s
    }
}

// ---------- history ----------

/// `n` zaken started at random moments in the last `days` days. Steps that would lie in
/// the future are left out, so recent zaken stay open. Returns the events in time order.
fn backfill(w: &mut World, n: usize, days: i64) -> Vec<(DateTime<Utc>, Ev)> {
    let now = Utc::now();
    let mut out = Vec::new();
    for _ in 0..n {
        let start = now - Mins::minutes(w.rng.gen_range(60..days.max(1) * 24 * 60));
        let mut t = start;
        for e in w.scenario(start).steps {
            t += Mins::minutes(e.after);
            if t > now {
                break;
            }
            out.push((t, e));
        }
    }
    out.sort_by_key(|(t, _)| *t);
    out
}

// ---------- HTTP ----------

fn post(agent: &ureq::Agent, base: &str, e: &Ev, time: DateTime<Utc>) -> Result<(), String> {
    let body = json!({
        "specversion": "1.0",
        "id": uuid::Uuid::now_v7().to_string(),
        "source": e.actor.source,
        "type": e.kind,
        "subject": e.subject,
        "time": time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
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

fn print(e: &Ev, t: DateTime<Utc>) {
    let short = e
        .kind
        .trim_start_matches("nl.vng.")
        .replacen("gesprekken.", "gesprek:", 1);
    println!(
        "{}  {:<16} {:<44} {}",
        t.with_timezone(&Local).format("%d-%m %H:%M:%S"),
        e.actor.slug,
        short,
        e.label
    );
}

fn arg<T: std::str::FromStr>(args: &[String], name: &str, default: T) -> T {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

const HELP: &str = "openevents-sim [--base URL] [--backfill N] [--days N] [--rate EVENTS_PER_SEC]
               [--count N] [--active N] [--seed N]

Sends semi-realistic events (meldingen openbare ruimte, aanvragen omgevingsvergunning
and the gesprekken that go with them) to an OpenEvents in dev mode with the example
profiles installed.

  --base      default http://127.0.0.1:8080
  --backfill  first write N zaken as history, with event times in the past (default 0)
  --days      how far back the history goes (default 30)
  --rate      live events per second (default 0.5; 0 = as fast as possible)
  --count     stop after N live events (default: never; 0 = only the history)
  --active    zaken running live at the same time (default 6)
  --seed      random seed (default: time based)";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{HELP}");
        return;
    }
    let base: String = arg(&args, "--base", "http://127.0.0.1:8080".to_string());
    let base = base.trim_end_matches('/').to_string();
    let rate: f64 = arg(&args, "--rate", 0.5);
    let count: u64 = arg(&args, "--count", u64::MAX);
    let active: usize = arg(&args, "--active", 6).max(1);
    let seed: u64 = arg(&args, "--seed", Utc::now().timestamp_millis() as u64);
    let backfill_n: usize = arg(&args, "--backfill", 0);
    let days: i64 = arg(&args, "--days", 30);

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(10))
        .build();
    match agent.get(&format!("{base}/profiles")).call() {
        Ok(r) => {
            let text = r.into_string().unwrap_or_default();
            for p in [
                "nl.vng.meldingen",
                "nl.vng.vergunningen",
                "nl.vng.gesprekken",
            ] {
                if !text.contains(&format!("\"{p}\"")) {
                    eprintln!(
                        "Profiel {p} ontbreekt. Start de server met OPENEVENTS_PROFILES=examples/profiles."
                    );
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("Kan {base} niet bereiken: {e}");
            std::process::exit(1);
        }
    }

    let mut w = World::new(seed);
    let mut failed = 0u64;
    if backfill_n > 0 {
        let history = backfill(&mut w, backfill_n, days);
        eprintln!(
            "historie: {backfill_n} zaken, {} events over {days} dagen",
            history.len()
        );
        let mut ok = 0;
        for (t, e) in &history {
            match post(&agent, &base, e, *t) {
                Ok(()) => {
                    ok += 1;
                    print(e, *t);
                }
                Err(msg) => {
                    failed += 1;
                    eprintln!("FOUT {} ({}): {msg}", e.kind, e.label);
                }
            }
        }
        eprintln!("historie klaar: {ok}/{} geschreven", history.len());
    }

    if count == 0 {
        return;
    }
    eprintln!("live: {rate} events/s, {active} zaken tegelijk, seed {seed} (Ctrl+C stopt)");
    let mut cases: Vec<Seq> = Vec::new();
    let mut sent = 0u64;
    while sent < count {
        while cases.len() < active {
            cases.push(w.scenario(Utc::now()));
        }
        let i = w.rng.gen_range(0..cases.len());
        let Some(e) = cases[i].steps.pop_front() else {
            cases.swap_remove(i);
            continue;
        };
        let now = Utc::now();
        match post(&agent, &base, &e, now) {
            Ok(()) => {
                sent += 1;
                print(&e, now);
            }
            Err(msg) => {
                // The rest of this zaak depends on this event: drop it.
                cases[i].steps.clear();
                failed += 1;
                eprintln!("FOUT {} ({}): {msg}", e.kind, e.label);
                if failed > 20 && failed > sent {
                    eprintln!("Te veel fouten, gestopt.");
                    std::process::exit(1);
                }
            }
        }
        if cases[i].steps.is_empty() {
            cases.swap_remove(i);
        }
        if rate > 0.0 {
            let pause = w.rng.gen_range(0.3..1.7) / rate;
            std::thread::sleep(Duration::from_secs_f64(pause));
        }
    }
    eprintln!("klaar: {sent} live events, {failed} fouten");
}
