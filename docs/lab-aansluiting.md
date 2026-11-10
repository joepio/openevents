# Aansluiting op het VNG API Lab

OpenEvents moet naadloos aansluiten op de patterns van het [VNG API Lab](https://github.com/VNG-Realisatie/vng-api-lab). Die patterns mogen daarvoor veranderen, ook de events-spec zelf, maar **alleen in `next`**. Benoemde versies (`v0.0.1` enzovoort) blijven zoals ze zijn.

Het idee is dat spec en code gelijk oplopen: elke wijziging in het lab heeft een implementatie in OpenEvents en een conformance-test, en elke functie van OpenEvents die in een contract hoort, staat in het lab.

## Welke patterns OpenEvents gebruikt

| Lab | Stand (10 november 2026) | Rol in OpenEvents |
|---|---|---|
| ADR-0002 snapshot + SSE | voorgesteld | leesmodel: `/query` als snapshot, `GET /events` als tail, `Last-Event-ID` als cursor |
| ADR-0003 begrensde query-grammatica | voorgesteld | basis voor de filtertaal; OpenEvents breidt uit (zie onder) |
| ADR-0004 ingest via CloudEvents | voorgesteld | schrijfmodel: `POST /events`, idempotent op `source` + `id` |
| ADR-0006 per-domein registers | voorgesteld | plaats van OpenEvents: een register per domein, geen landelijke kluis |
| `schemas/events` v0.0.1 | op `main` | CloudEvents-envelop met `sequence` en `dataref` |
| `schemas/events` next | in PR #47 en #49 | envelop plus JSON Commit-velden |
| `patterns/sync-feed` next | op `main` | `feedCursor` die vóór de snapshot wordt vastgesteld, `redact`-records, opake cursors |
| `patterns/event-stream` next | in PR #47 | SSE-contract |
| `patterns/json-commit` next | in PR #49 | `POST /events` met `json.commit.transaction` |
| `patterns/query` next | schets in PR #53 | `POST /query`; de filters gaan van CloudEvents-dialecten naar CQL2-JSON |
| `patterns/file-upload` next | op `main` | bijlagen via `dataref` |
| `patterns/error-responses` v0.0.1 | op `main` | `problem+json` |
| `patterns/federated-auth` next | op `main` | tokens van buiten, geen eigen login |
| issue #16 patroon voor queryen | open | beantwoord door [query.md](query.md): POST nu, `QUERY` later, live via SSE |
| issue #51 autorisatie | open | [autorisatie.md](autorisatie.md): autorisatie als query, zonder externe beslisser; vervangt het `context`-voorstel van 3 november |

## Voorgestelde wijzigingen in `next`

Deze lijst is het werkpakket voor het lab. Elke regel wordt een commit in een lab-PR, samen met de bijbehorende code hier.

### `schemas/events` next

1. **Kenmerk `automatic`** (boolean) voor afgeleide events, zoals het automatische antwoord bij Gesprekken.
2. **Standaardtype voor wissen:** `nl.vng.openevents.subject.erased` als tombstone, in lijn met `redact` uit sync-feed. Eén begrip in beide patterns; voorstel is de term `redact` aan te houden en het eventtype `subject.redacted` te noemen.
3. **`retainUntil`** als optionele extensie op het eerste event van een onderwerp.
4. **`aanleidingzaak` vervangen** (extensie uit de MijnGesprekken-PoC) door een algemene manier om verwijzingen op te nemen die het register kan indexeren. Voorstel: niet als CloudEvents-extensie, maar via het profiel (`references`), zodat de envelop generiek blijft.

### `patterns/event-stream` next

1. **Filter op `GET /events`** in dezelfde taal als `/query` (body via `POST /events/subscriptions` of een `filter`-parameter met alleen niet-gevoelige waarden).
2. **`Last-Event-ID` verplicht ondersteunen**, en een `cursor`-parameter als alternatief voor clients zonder header.
3. **Stream-tickets** (`POST /events/tickets`) zodat tokens niet in URL's staan.
4. **Meldingen** `subject.matched`, `subject.unmatched`, `auth.expiring` als vaste systeemevents.

### `patterns/query` next (PR #53)

0. **Filtertaal naar CQL2-JSON** (OGC) met een begrensd profiel, inclusief tijd en ruimte, en de functie `actor()` voor regels. Dezelfde taal voor `GET /events`-abonnementen. Dit sluit aan op lab issue #51, dat CQL2-JSON al noemde.
1. **Paden** in `property` over verwijzingen, maximaal 3 stappen.
2. **`fields` en `include`** voor representaties, met de grenzen uit [query.md](query.md).
3. **Benoemde views:** `GET`/`POST /views/{naam}`.
4. **`feedCursor`** in elke respons, met de semantiek uit sync-feed.
5. **Autorisatie is een query:** regels in dezelfde filtertaal met `actor()`, altijd met AND bij de query van de client, vóór paginering; tellingen alleen binnen rechten; verborgen velden zien eruit als afwezige. Dit wordt ook het voorstel voor lab issue #51.
6. **Kostengrens:** `problem+json` type `query-too-expensive`.

### Nieuwe ADR

**ADR-0008: krachtige query bij een register.** Vult ADR-0003 aan: ADR-0003 blijft gelden voor `/zoek` per bron-API (laag instapniveau voor leveranciers); bij een register per domein (ADR-0006) mag de rijkere taal uit `patterns/query`, omdat één component de engine, de indexen en de autorisatie levert.

### Profielen als lab-onderwerp

Een profiel (eventtypes, dataschema's, projectieregels, beleid) is zelf ook een contract. Voorstel: `profiles/gesprekken/next.yaml` in het lab, naast `apis/rest/gesprekken`, zodat de vergelijking "API of profiel" in het lab zichtbaar is.

## Volgorde

1. Eerst de lopende lab-PR's: #47 (Gesprekken, event-stream), daarna #49 (JSON Commit) rebasen. #53 blijft een schets tot OpenEvents een eerste query-engine heeft.
2. Daarna één lab-PR per onderwerp hierboven, elk met een verwijzing naar de implementatie hier.
