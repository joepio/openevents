# Architectuur

## CQRS: OpenEvents is de leeskant

De architectuur is CQRS met een eventlog ertussen.

- **Commands** (beslissen of iets mag volgens de domeinregels) vallen **buiten** OpenEvents. Die zitten bij de bronnen: de Gesprekken API, OneGround, Open Zaak, een app, of een kleine domeinservice.
- **Events** zijn feiten. OpenEvents neemt ze aan en controleert alleen wat nodig is om het log betrouwbaar te houden: schema, schrijfregel, idempotentie en `previousCommit`.
- **Queries** en de stroom zijn de kern van OpenEvents: projecties, indexen, `/query`, views en SSE.

Een profiel beschrijft hoe events eruitzien, hoe ze tot een stand leiden en wie wat mag zien, **maar niet wat er moet gebeuren**. Reacties op events (een automatisch antwoord, een gesprek sluiten na 3 dagen, een klantcontact aanmaken) zijn consumers die de stroom volgen en zelf events schrijven.

## Plaats in het landschap

OpenEvents is een **register per domein** in de zin van [ADR-0006 van het lab](https://github.com/VNG-Realisatie/vng-api-lab/blob/main/architecture/adr/0006-per-domein-registers.md). Een gemeente of haar leverancier draait het, voor een domein zoals gesprekken of een tijdlijn bij zaken. Het is geen landelijke kluis.

```
 bronnen (Open Zaak, OneGround, Open Klant, apps)
        │  POST /events  (CloudEvents / JSON Commit)
        ▼
 ┌──────────────────────── OpenEvents ────────────────────────┐
 │  ingest: authn, validatie, beleid, transactie              │
 │     │                                                      │
 │     ▼                                                      │
 │  log (append-only, volgnummer)  ──►  projecties (stand)    │
 │     │                                 │                    │
 │     │                                 ▼                    │
 │     │                           indexen (velden, refs,     │
 │     │                           rechten, tekst)            │
 │     ▼                                 │                    │
 │  GET /events (SSE)            POST /query                  │
 └────────────────────────────────────────────────────────────┘
        │                                 │
        ▼                                 ▼
 consumers (portaal, medewerkerconsole, e-mail, push, archief)
```

## Begrippen

| Begrip | Betekenis |
|---|---|
| **Event** | Een CloudEvent: `id`, `source`, `type`, `subject`, `time`, `dataschema`, `data`. Onveranderlijk, op het versleutelen na (zie [wissen](wissen.md)). |
| **Onderwerp** (`subject`) | De URI van waar het event over gaat, bijvoorbeeld een gesprek of een zaak. Het onderwerp is de eenheid van ordening, autorisatie en wissen. |
| **Log** | Alle events op volgorde, met een globaal volgnummer (`sequence`) dat ook de SSE-cursor is. |
| **Projectie** | De actuele stand van een onderwerp, afgeleid uit zijn events volgens de regels van het profiel. Dit is wat `/query` teruggeeft. |
| **Profiel** | Een domein op het register: eventtypes, dataschema's, projectieregels en beleid. Zie [profielen](profielen.md). |
| **Transactie** | Een JSON Commit: één of meer wijzigingen die samen slagen of samen falen. |

## Opslag

De kern draait op een key-value-store achter een trait, naar het voorbeeld van `KvStore` in atomic-server: `get`, `insert`, `remove`, `scan_prefix`, `range` en een atomaire `apply_batch`. Daarmee kan een implementatie kiezen tussen een embedded store (eenvoudig hosten) en een gedeelde store (failover), zonder dat de rest van de code verandert. Zie [ADR-0002](adr/0002-opslag-op-key-value.md).

Tabellen (prefixes) in de store:

| Tabel | Sleutel | Waarde |
|---|---|---|
| `log` | `sequence` | versleuteld event |
| `subject_log` | `subject` + `sequence` | leeg (index voor de tijdlijn) |
| `idempotency` | aanroeper + `source` + `id` | resultaat van de eerste aanroep |
| `projection` | `subject` | actuele stand (JSON), versleuteld |
| `head` | `subject` | laatste commit-id (voor `previousCommit`) |
| `idx_field` | profiel + veld + waarde + `subject` | leeg |
| `idx_ref` | doel + veld + bron | leeg (omgekeerde verwijzingen, voor paden) |
| `keys` | `subject` | sleutel voor die onderwerp-inhoud |
| `profiles` | profiel-id + versie | profieldefinitie |

Alle schrijfacties voor één transactie (log, projectie, indexen, head, idempotentie) gaan in **één** `apply_batch`. Een bevestigde write is duurzaam (eis K1): de store moet bij bevestiging gesynchroniseerd hebben.

## Schrijfpad

1. **Authenticatie.** OIDC-token; de agent is een persoon of een systeem (eis A6, A7).
2. **Parsen.** CloudEvent of JSON Commit-envelop (`type: json.commit.transaction`).
3. **Idempotentie.** Bestaat aanroeper + `source` + `id` al, dan komt het eerdere resultaat terug.
4. **Validatie.** Het `type` moet bij een geregistreerd profiel horen; `data` wordt gevalideerd tegen het `dataschema` (eis F11).
5. **Beleid.** Komt het onderwerp door de `append`- of `write`-regel van het profiel voor deze agent? Dat is een query op dezelfde engine (zie [autorisatie](autorisatie.md)).
6. **Concurrency.** Klopt `previousCommit` met `head`? Zo niet, 412.
7. **Projectie.** De projectieregels van het profiel berekenen de nieuwe stand. Projectie is puur: hij maakt geen nieuwe events en roept niets buiten het register aan.
8. **Opslaan.** Eén batch.
9. **Uitzenden.** Het event gaat naar de interne broadcast; elke SSE-verbinding filtert en autoriseert zelf.

Alles wat een beslissing of neveneffect is (een automatisch antwoord, automatisch sluiten, een klantcontact in Open Klant, een e-mail) is een **consumer**: ze volgen de stroom met een eigen cursor en doen hun werk at-least-once. Zo blijft de transactie klein en het register onafhankelijk van andere systemen.

## Leespad

- `GET /events` (SSE): filter (dezelfde taal als `/query`), cursor via `Last-Event-ID`, per event de leesregel van het profiel en de veldregel. Het token gaat in een header; voor browsers die geen headers bij `EventSource` kunnen meesturen, komt een kortlevend stream-ticket via `POST /events/tickets`.
- `GET /events?format=json` (of `Accept: application/json`): dezelfde selectie als pagina's.
- `GET /subjects/{subject}/events`: de tijdlijn van één onderwerp.
- `POST /query`: zie [query-engine](query.md).
- `GET /profiles`, `GET /schemas/...`: wat het register kent. Een AsyncAPI-document wordt uit de profielen gegenereerd.

## API-oppervlak (eerste versie)

| Methode | Pad | Doel |
|---|---|---|
| `POST` | `/events` | schrijven (CloudEvent of JSON Commit) |
| `GET` | `/events` | volgen (SSE) of pagineren (JSON) |
| `POST` | `/events/tickets` | kortlevend ticket voor SSE vanuit de browser |
| `GET` | `/subjects/{subject}/events` | tijdlijn van één onderwerp |
| `GET` | `/subjects/{subject}` | projectie van één onderwerp |
| `POST`, `QUERY` | `/query` | bevragen |
| `GET` | `/views/{naam}` | benoemde representatie, met alleen niet-gevoelige parameters |
| `POST` | `/views/{naam}` | benoemde representatie met gevoelige parameters in de body |
| `GET`, `PUT` | `/profiles/{id}` | profielen lezen en registreren (beheer) |
| `POST` | `/subjects/{subject}/erase` | wissen (beheer, zie [wissen](wissen.md)) |
| `GET` | `/schemas/...`, `/asyncapi.json`, `/openapi.json` | contracten |

## Wat uit ZaakChat komt

| ZaakChat | OpenEvents |
|---|---|
| `src/commits.rs`: JSON Commit, idempotentie, `previousCommit`, subject-binding | vrijwel direct over te nemen |
| SSE met snapshot plus deltas (`get_or_stream_events`) | over te nemen, met `Last-Event-ID` en het token uit de URL |
| `resource_aggregates`, `resource_heads` | wordt `projection` en `head` |
| Tantivy voor zoeken | optioneel, alleen voor vrije tekst; filters en paden gaan via eigen indexen |
| `involved`-autorisatie, herkenning van issues aan velden | vervangen door lees- en schrijfregels in het profiel |
| Rust-types als schema's | vervangen door profielen tijdens runtime |
| e-mail, push, frontend | worden aparte consumers; ZaakChat wordt de eerste app op OpenEvents |

## Een zaaksysteem op OpenEvents

Wie een echt zaaksysteem wil maken, krijgt deze rolverdeling:

| Onderdeel | Waar het zit |
|---|---|
| Zaaktype-logica: statusovergangen, verplichte gegevens, besluiten, termijnen | **modules** in de modulehost (command), zie [modules](modules.md) |
| Wie welke handeling mag doen | de module, voor commands; OpenEvents controleert alleen de schrijfregel |
| Opslag, geschiedenis, audittrail | **OpenEvents** (log) |
| Actuele stand van een zaak | OpenEvents (projectie, profiel `zaken`) |
| Werkvoorraad, "mijn zaken", zoeken, kaart, tijdlijn | OpenEvents (`/query`, views, SSE) |
| Wie wat mag lezen | OpenEvents (leesregels) |
| Brieven, klantcontact, archief, notificaties | consumers |
| Zaaktypen en catalogus | definities als data; een kandidaat is Atomic Data, dat sterk is in gedeelde schema's |
| ZGW-API's voor bestaande afnemers | een façade: writes worden commands, reads worden `/query` |

Een handeling: een behandelaar kiest "zet status op Besluit genomen". De module leest de zaak, toetst de regels en schrijft één JSON Commit met `previousCommit`, bijvoorbeeld `status.gezet`, `besluit.genomen` en `resultaat.bepaald`, atomair. Dat zijn de "handelingen" waar VNG Platform Dienstverlening om vraagt, zonder een nieuwe API per handeling.

Als OpenEvents de bron wordt (system of record), worden deze eisen verplicht in plaats van later:

- duurzaam schrijven, online backup, herstel naar een tijdstip, failover (K1 tot K3) en schaal tot miljoenen zaken (K4);
- invarianten over onderwerpen heen, zoals een unieke zaakidentificatie: een uniekheidsindex of een teller in de transactie, want `previousCommit` geldt per onderwerp;
- documenten via `dataref` naar opslag met dezelfde leesregels;
- Archiefwet: overbrengen naar een e-depot en vernietigen, met een bewijs als event.
