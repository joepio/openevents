# OpenEvents

OpenEvents is een eventregister met een query-engine. Bronnen schrijven gebeurtenissen (CloudEvents) naar één plek. Clients volgen ze live via SSE en vragen de actuele stand op met één krachtige, geautoriseerde query.

> **Status: ontwerp.** Er is nog geen code. Dit repo bevat eerst het ontwerp, zodat het ontwerp en de VNG API Lab-patterns gelijk kunnen oplopen.

## Waarom

Vandaag heeft elke API zijn eigen manier om wijzigingen door te geven: ZGW-notificaties via webhooks, losse `/events`-endpoints, polling. Niemand bewaart de geschiedenis, er is geen replay, en wie een tijdlijn over meerdere registers wil, moet zelf alles aan elkaar knopen. Bevragen is net zo versnipperd: elke API heeft eigen filters, JOINs over registers kunnen niet, en autorisatie is grof.

Tijdens het fieldlab MijnGesprekken (10 november 2026) bleek er belangstelling voor één component dat dit generiek oplost. Een gesprek is in feite een reeks events op één onderwerp. Als het register dat goed kan, hoeft een domein zoals Gesprekken alleen nog een **profiel** te zijn: eventtypes, schema's en regels.

## In één oogopslag

| Wat | Hoe |
|---|---|
| Schrijven | `POST /events`: één CloudEvent of een JSON Commit-transactie (atomair, idempotent, optimistic concurrency) |
| Volgen | `GET /events` als SSE, met filter en `Last-Event-ID` als cursor |
| Bevragen | `POST /query` (later ook de HTTP-methode `QUERY`): filters, paden over verwijzingen, representaties met `include`, benoemde views |
| Live bevragen | een query geeft een snapshot plus `feedCursor`; daarna volg je dezelfde selectie via `GET /events` |
| Profielen | eventtypes, dataschema's, projectieregels en beleid, tijdens runtime te registreren |
| Autorisatie | een recht is een query: per type een leesregel in dezelfde filtertaal, met `$actor` uit het token en paden naar teams en deelnames; altijd met AND bij de query, vóór paginering, ook op de stroom |
| Wissen | crypto-shredding per onderwerp, plus tombstone-events |
| Opslag | eigen kern in Rust op een key-value-store achter een trait |

## Documentatie

- [Eisen](docs/eisen.md): wat het moet kunnen, als eigenschappen
- [Architectuur](docs/architectuur.md): log, projecties, indexen, API
- [Query-engine](docs/query.md): taal, paden, representaties, live
- [Autorisatie](docs/autorisatie.md): autorisatie als query, regels in het profiel, velden per rol
- [Wissen en bewaren](docs/wissen.md): AVG, Archiefwet, crypto-shredding
- [Profielen](docs/profielen.md): hoe een domein erop landt, met Gesprekken als eerste
- [Aansluiting op het VNG API Lab](docs/lab-aansluiting.md): welke patterns, en wat daar in `next` moet veranderen
- [Roadmap](docs/roadmap.md)
- [Besluiten (ADR's)](docs/adr/)

## Herkomst

OpenEvents leent van twee bestaande projecten:

- **[ZaakChat](https://github.com/joepio/zaakchat):** JSON Commit op `POST /events`, SSE met snapshot en deltas, projecties naast het log, schema-endpoints, één binary zonder externe database.
- **Atomic Data / atomic-server:** schema's als data, een `KvStore`-trait, een index op verwijzingen, plugins met `before_commit`-hooks.

Zie [ADR-0001](docs/adr/0001-eigen-kern-in-rust.md) voor waarom het een eigen kern wordt en geen uitbreiding van een van beide.
