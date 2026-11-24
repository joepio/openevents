# OpenEvents

OpenEvents is een eventregister met een query-engine. Bronnen schrijven gebeurtenissen (CloudEvents) naar één plek. Clients volgen ze live via SSE en vragen de actuele stand op met één krachtige, geautoriseerde query.

> **Status: fase 1 in aanbouw.** Er is een eerste werkende kern (zie [Draaien](#draaien)). Het ontwerp staat in [`docs/`](docs/) en loopt gelijk op met de VNG API Lab-patterns.

## Waarom

Vandaag heeft elke API zijn eigen manier om wijzigingen door te geven: ZGW-notificaties via webhooks, losse `/events`-endpoints, polling. Niemand bewaart de geschiedenis, er is geen replay, en wie een tijdlijn over meerdere registers wil, moet zelf alles aan elkaar knopen. Bevragen is net zo versnipperd: elke API heeft eigen filters, JOINs over registers kunnen niet, en autorisatie is grof.

Tijdens het fieldlab MijnGesprekken (10 november 2026) bleek er belangstelling voor één component dat dit generiek oplost. Een gesprek is in feite een reeks events op één onderwerp. Als het register dat goed kan, hoeft een domein zoals Gesprekken alleen nog een **profiel** te zijn: eventtypes, schema's en regels.

## In één oogopslag

| Wat | Hoe |
|---|---|
| Schrijven | `POST /events`: één CloudEvent of een JSON Commit-transactie (atomair, idempotent, optimistic concurrency) |
| Volgen | `GET /events` als SSE, met filter en `Last-Event-ID` als cursor |
| Bevragen | `POST /query` (later ook de HTTP-methode `QUERY`): CQL2-filters met tijd en ruimte, paden over verwijzingen, representaties met `include`, benoemde views |
| Live bevragen | een query geeft een snapshot plus `feedCursor`; daarna volg je dezelfde selectie via `GET /events` |
| Profielen | types, handelingen en regels, tijdens runtime te registreren; elk event is een commit ([ADR-0008](docs/adr/0008-events-zijn-commits.md)) |
| Autorisatie | een recht is een query: per type een leesregel in dezelfde filtertaal, met `actor()` uit het token en paden naar teams en deelnames; altijd met AND bij de query, vóór paginering, ook op de stroom |
| Wissen | crypto-shredding per onderwerp, plus tombstone-events |
| Modules | domeinlogica (bezwaar, melden openbare ruimte) als kleine open WASM-modules naast het register: commands, reacties, timers, formulieren |
| Opslag | eigen kern in Rust op een key-value-store achter een trait |

## Documentatie

- [Aansluiten als leverancier](docs/aansluiten.md): events posten, tokens, batch, OpenAPI, of via ZGW-notificaties
- [Eisen](docs/eisen.md): wat het moet kunnen, als eigenschappen
- [Architectuur](docs/architectuur.md): log, stand, indexen, API
- [Query-engine](docs/query.md): taal, paden, representaties, live
- [Autorisatie](docs/autorisatie.md): autorisatie als query, regels in het profiel, velden per rol
- [Beheer](docs/beheer.md): backup en herstel, `/health`, `/metrics`
- [Wissen en bewaren](docs/wissen.md): definitief verwijderen, bewaartermijnen, BSN-pseudoniemen
- [Profielen](docs/profielen.md): hoe een domein erop landt, met Gesprekken als eerste
- [Modules](docs/modules.md): domeinlogica als kleine open modules, met bezwaar als voorbeeld
- [MijnOverheid-demo op OpenEvents](docs/mijnoverheid.md): MijnZaken en MijnTaken als leesweergave, live, en een portaal op `/mijnoverheid` dat alleen events kent
- [Aansluiting op het VNG API Lab](docs/lab-aansluiting.md): welke patterns, en wat daar in `next` moet veranderen
- [Roadmap](docs/roadmap.md)
- [Besluiten (ADR's)](docs/adr/)

## Herkomst

OpenEvents leent van twee bestaande projecten:

- **[ZaakChat](https://github.com/joepio/zaakchat):** JSON Commit op `POST /events`, SSE met snapshot en deltas, projecties naast het log, schema-endpoints, één binary zonder externe database.
- **Atomic Data / atomic-server:** schema's als data, een `KvStore`-trait, een index op verwijzingen, bewaakte query's.

Zie [ADR-0001](docs/adr/0001-eigen-kern-in-rust.md) voor waarom het een eigen kern wordt en geen uitbreiding van een van beide.

## Draaien

```sh
# lokaal, met demo-tokens en de voorbeeldprofielen
OPENEVENTS_DEV_AUTH=true OPENEVENTS_ADMINS=urn:admin OPENEVENTS_PROFILES=examples/profiles \
  cargo run -p openevents-server

# tests
cargo test
```

Open daarna http://localhost:8080/mijnoverheid voor een klein inwonersportaal op alleen events, of http://localhost:8080/ui voor de beheer-UI: stand (materialized state), events (live), queries en schrijven. Met `OPENEVENTS_DEV_AUTH=true` kies je rechtsboven een persona uit de profielen ("Bekijk als", bijvoorbeeld inwoner of KCC-medewerker) en zie je wat die persoon mag zien; het paneel "Wat ziet wie?" vergelijkt alle persona's voor één subject. Zonder dev-modus plak je een token. Demodata laden: `examples/seed.sh` (tegen `http://localhost:8080`, of geef een andere basis-URL mee).

Een stroom semi-realistische zaakevents: meldingen openbare ruimte (binnen via Signalen of het KCC), aanvragen omgevingsvergunning (via het Omgevingsloket) en de gesprekken daarbij, gekoppeld met `aanleiding.zaak`. Eerst 30 dagen historie, daarna live:

```sh
cargo run -p openevents-sim -- --base http://localhost:8080 --backfill 40 --rate 0.5
```

Opties: `--backfill N` (zaken als historie, met tijden in het verleden; recente zaken blijven open), `--days`, `--rate` (live events per seconde, 0 is zo snel mogelijk), `--count` (0 is alleen historie), `--active` (zaken tegelijk) en `--seed`. Elke actor post met zijn eigen dev-token, dus de regels van de profielen gelden net als voor echte clients. Kies in de UI bijvoorbeeld Ahmed (team buitenruimte), Femke (vergunningen), Priya of Jan (inwoners), Karel (ziet meldingen van anderen zonder melder) of Kim (KCC, beperkte velden).

Of met Docker:

```sh
docker build -t openevents .
docker run -p 8080:8080 -v openevents:/data \
  -e OPENEVENTS_JWT_SECRET=... -e OPENEVENTS_ADMINS=urn:admin -e OPENEVENTS_PROFILES=/profiles openevents
```

| Variabele | Betekenis |
|---|---|
| `OPENEVENTS_DATA` | map voor de opslag (standaard `data`) |
| `OPENEVENTS_ADDR` | adres (standaard `0.0.0.0:8080`) |
| `OPENEVENTS_JWT_SECRET` | geheim voor HS256-tokens; `sub` is de actor, alle claims zijn beschikbaar in `actor('claim')` |
| `OPENEVENTS_DEV_AUTH` | `true` staat `Bearer dev:<actor>\|claim=waarde` toe, alleen voor demo's en tests |
| `OPENEVENTS_ADMINS` | actors (komma's) die profielen mogen registreren |
| `OPENEVENTS_PROFILES` | map met profielen (`.json`) die bij het starten worden geïnstalleerd |
| `OPENEVENTS_OIDC_ISSUER`, `…_JWKS_URL`, `…_AUDIENCE`, `OPENEVENTS_ACTOR_CLAIM`, `OPENEVENTS_ACTOR_PREFIX`, `OPENEVENTS_UI_CLIENT_ID` | tokens van een eigen IdP (OAuth2 client credentials voor leveranciers), zie [docs/aansluiten.md](docs/aansluiten.md) |
| `OPENEVENTS_SCHEMAS` | map met lokale kopieën van gedeelde schema's (`host/pad`); standaard `schemas` naast `OPENEVENTS_PROFILES`, zie [docs/profielen.md](docs/profielen.md) |
| `OPENEVENTS_NOTIFICATIES_AUTH`, `OPENEVENTS_ZGW_CLIENT_ID`, `OPENEVENTS_ZGW_SECRET` | ZGW-notificaties als events binnenhalen via `POST /notificaties`, zie [docs/aansluiten.md](docs/aansluiten.md) |

Een gesprek starten en volgen:

```sh
T="Authorization: Bearer dev:https://id.example.nl/suzanne"
curl -s localhost:8080/events -H "$T" -H 'content-type: application/json' -d '{
  "specversion": "1.0", "id": "e1", "source": "urn:demo", "subject": "urn:uuid:g1",
  "type": "nl.vng.gesprekken.gesprek.gestart",
  "data": { "type": "nl.vng.gesprekken.gesprek", "resourceData": {
    "onderwerp": "Vraag", "deelnames": [ { "partij": "https://id.example.nl/suzanne", "rol": "inwoner" } ] } } }'
curl -N localhost:8080/events -H "$T" -H 'accept: text/event-stream' -H 'last-event-id: 0'
curl -s localhost:8080/query -H "$T" -H 'content-type: application/json' \
  -d '{ "type": "nl.vng.gesprekken.gesprek", "filter": "status = '"'"'open'"'"'" }'
```

### Wat er in fase 1 zit

- KV-trait met redb (synchroon schrijven) en een in-memory variant
- `POST /events`: elk event is een commit (`resourceData`, `patch` of `deleted`) en het type benoemt de handeling uit het profiel; meerdere commits in `data.commits`, alles of niets; idempotent op `source` + `id`, `previousCommit` met 412
- profielen tijdens runtime (`PUT /profiles/{id}`): types (JSON Schema, regels, velden per rol) en handelingen (welke commits, welke velden, wat de server zet, wie het mag); regels en filters volgen verwijzingen (`gesprek.status`)
- lees-, maak-, schrijf- en toevoegregels in CQL2-tekst of CQL2-JSON (`=`, `<>`, `<`, `<=`, `>`, `>=`, `IN`, `IS NULL`, `AND`, `OR`, `NOT`, `actor()`, `actor('claim')`)
- `GET /events` als SSE (`Last-Event-ID`, tickets) of JSON, `GET /subjects/{subject}`, `GET /subjects/{subject}/events`, `/related` (wat ernaar verwijst) en `/audit` (wie deed wat, over de zaak en alles eromheen)
- veldregels (`rules.fields` met `when` en `show`): wie een subject alleen beperkt mag zien, krijgt alleen de genoemde velden en events zonder `data` (`dataHidden: true`)
- persona's in profielen en een beheer-UI op `/ui`
- OIDC: tokens van een IdP via JWKS (discovery, sleutelrotatie), naast HS256 en dev-tokens
- batch-ontvangst (`application/cloudevents-batch+json`) en een OpenAPI per profiel (`/profiles/{id}/openapi.json`)
- `POST /notificaties`: ZGW-notificaties worden events, met een zaakspiegel als voorbeeldprofiel
- `POST /query` met filter, `fields`, paginering, `feedCursor` en `mine` (wat over de aanroeper gaat, volgens de `mine`-regel in het profiel; ook als attribuut op de eventstroom); met veldindexen voor `=` en `IN` op de queryable velden, de verwijzingen en de velden van de `mine`-regel (andere filters scannen het type), nog zonder paden over verwijzingen en zonder `include`
- privacy: `DELETE /subjects/{subject}` (met `cascade`), bewaartermijnen per type (`retention`), BSN als pseudoniem (`OPENEVENTS_PSEUDONYM_KEY`)

Nog niet: paden over verwijzingen, `include`, views, crypto-shredding voor backups, modules.
