# ADR-0003: Query: krachtig, maar begrensd op diepte, kosten en velden

- **Status:** Voorgesteld
- **Datum:** 2026-11-10

## Context

Lab ADR-0003 koos een begrensde grammatica voor `/zoek` per API, omdat elke leverancier hem zelf moest bouwen en autoriseren. Het register heeft echter alles in huis voor meer: de stand van alle onderwerpen, indexen op velden en verwijzingen, en één plek voor autorisatie. De wens is dat je er representaties uit kunt halen, geautoriseerd.

## Beslissing

`POST /query` (later ook `QUERY`) met CQL2-JSON als filtertaal (ADR-0006), uitgebreid met paden over verwijzingen, `fields`, `include` en benoemde views. Elke query heeft vaste grenzen en een kostenschatting vooraf. Elke respons heeft een `feedCursor` zodat de client live verder kan via `GET /events`.

Zie [query.md](../query.md) voor de details.

## Alternatieven

- **Alleen de grammatica van lab ADR-0003.** Te beperkt: geen paden, dus geen "zaken waar ik betrokkene ben", en geen `include`, dus N+1.
- **GraphQL.** Goede veldselectie en relaties, maar per-veld-autorisatie en kostenbeheersing zijn lastig, en het deelt geen taal met abonnementen. Blijft kandidaat als tweede ingang op dezelfde engine.
- **Vrije zoektaal (Elasticsearch-DSL, Tantivy-syntax zoals in ZaakChat).** Niet te autoriseren op een geparste boom, en een DoS-oppervlak.

## Gevolgen

- Eén taal voor query, abonnement en autorisatie (zie ADR-0004).
- Lab krijgt een ADR-0008 die ADR-0003 aanvult voor registers (zie [lab-aansluiting](../lab-aansluiting.md)).
- Live query's met paden zijn in de eerste versie benaderend (meldingen `subject.matched`/`unmatched`, client vraagt opnieuw).
