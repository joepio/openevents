# Roadmap

Elke fase eindigt met iets dat te laten zien is, en met de bijbehorende wijziging in het lab.

## Fase 0: ontwerp (nu)

- Dit repo: eisen, architectuur, query, autorisatie, wissen, profielen, ADR's.
- Reactie op lab issue #16 en uitbreiding van de schets in lab PR #53.

## Fase 1: kern (2 tot 3 weken)

**Stand 10 november 2026:** de eerste versie van de kern staat in `crates/` (zie de README, "Wat er in fase 1 zit"). OIDC met JWKS, batch, OpenAPI per profiel en de Notificaties-adapter staan erin (zie [aansluiten.md](aansluiten.md)). Sinds [ADR-0008](adr/0008-events-zijn-commits.md) is elk event een commit en benoemt het type de handeling; de projectietaal is weg, regels en filters volgen verwijzingen. Nog open in deze fase: de demo op DemoStack, conformance-tests als aparte set, een compatibiliteitscheck bij het wijzigen van profielen.

- Rust-crate `openevents-core`: KV-trait met een embedded implementatie, log, idempotentie, JSON Commit (overgenomen uit ZaakChat `commits.rs`), handelingen met commits (JSON Merge Patch), `head` en 412.
- `openevents-server`: `POST /events`, `GET /events` (SSE met `Last-Event-ID` en tickets), `GET /subjects/{subject}` en de tijdlijn.
- Profielen tijdens runtime met JSON Schema-validatie.
- Lees- en schrijfregels per type (eerst zonder paden, alleen `deelnames.partij = actor()`); OIDC-tokens.
- Conformance-tests in een aparte map, zodat ze ook tegen andere implementaties kunnen draaien.

**Demo:** de Gesprekken API (OneGround-fork) publiceert naar OpenEvents; de medewerkerconsole leest via SSE. Op DemoStack naast de MijnGesprekken-demo.

## Fase 2: query (2 tot 3 weken)

- `POST /query` met CQL2-JSON (crate `cql2`), paden, `fields` en `include`; ruimtelijke en tijdsindexen.
- Indexen op velden en verwijzingen; kostenschatting en grenzen.
- Views.
- `feedCursor` en live volgen met hetzelfde filter.
- Regels met paden (`zaaktype.team.leden`) en velden per rol; organisatieprofiel met teams en leden.

**Demo:** de werkvoorraad van een Wmo-consulent ("gesprekken bij mijn zaaktypen, met de laatste bijdrage en de zaak erbij") in één request, en "mijn open gesprekken" voor de inwoner, beide live.

## Fase 3: Gesprekken als profiel (2 weken)

- Profiel `nl.vng.gesprekken` met handelingen en lees- en schrijfregels.
- Consumers voor het automatische antwoord en het automatisch sluiten.
- Dunne REST-laag die het Gesprekken-contract (lab PR #47) op events vertaalt.
- Consumer voor klantcontact in Open Klant bij afsluiten.

**Demo:** dezelfde Open Inwoner-fork werkt tegen OpenEvents in plaats van tegen de eigen opslag van de Gesprekken API.

## Fase 3b: modules (3 weken)

- Modulehost met wasmtime: commands, reacties, timers, host-functies, sandbox (hergebruik uit atomic-server waar het kan).
- WIT-interface en een SDK voor Rust en JavaScript.
- Testrunner voor scenario's (gegeven, bij, verwacht).
- De Gesprekken-consumers (automatisch antwoord, sluiten) als eerste module.
- Bezwaarmodule als tweede, met formulieren voor portaal en console.

**Demo:** een bezwaar indienen in het portaal, de behandelaar ziet het in de werkvoorraad gesorteerd op termijn, verdagen en beslissen, alles live.

## Op DemoStack

OpenEvents komt als eigen onderdeel op DemoStack (`events.demostack.nl`), in `stacks/` van joepio/demostack, met dezelfde SSO (Keycloak) als de rest van de stack. ZaakChat wordt daar de voorbeeldapp: de React-frontend praat dan met OpenEvents in plaats van met zijn eigen backend, en laat zien wat `/query`, views, de stroom en de leesregels samen doen.

| Stap | Wanneer |
|---|---|
| OpenEvents-container naast de MijnGesprekken-demo, Gesprekken API publiceert ernaar | einde fase 1 |
| ZaakChat-frontend op OpenEvents (profiel `zaakchat`: zaken, berichten, taken) | fase 2, zodra `/query` en views er zijn |
| Zaakspiegel uit Open Zaak via notificaties, zodat ZaakChat echte DemoStack-zaken toont | fase 2 |
| Bezwaarmodule met het portaal en de medewerkerconsole | fase 3b |

## Fase 4: productierijp

- Wissen met crypto-shredding en bewaartermijnen.
- Online backup, herstel naar een tijdstip, een replica die het log volgt.
- Benchmark met 10 miljoen events.
- Beveiligingsreview.
- Een tweede profiel (bijvoorbeeld een zaakspiegel voor de tijdlijn over zaken, taken en gesprekken).

## Open vragen

- Wie draait OpenEvents in een pilot: de leverancier van het zaaksysteem, een samenwerkingsverband, of de gemeente zelf (lab ADR-0006)?
- Is één register per domein genoeg, of willen gemeenten één register voor alle "berichten bij een zaak"?
- Hoe verhoudt OpenEvents zich tot Open Notificaties op termijn? Er is nu een brug (`POST /notificaties`) die ZGW-notificaties als events binnenhaalt; vervangen is een latere vraag.
- Licentie: EUPL-1.2 of MIT?
