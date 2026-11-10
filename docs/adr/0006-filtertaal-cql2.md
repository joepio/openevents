# ADR-0006: Filtertaal is CQL2 met een begrensd profiel

- **Status:** Voorgesteld
- **Datum:** 2026-11-10

## Context

Eén filtertaal dient voor query's, abonnementen op de stroom en lees- en schrijfregels. Het eerste voorstel (lab PR #53) gebruikte de dialecten van de CloudEvents Subscriptions API. Modules zoals melden openbare ruimte vragen om ruimtelijke filters, termijnen om tijdsfilters. Lab issue #51 noemde CQL2-JSON al als kandidaat voor autorisatiefilters.

## Beslissing

OGC CQL2, met een profiel van toegestane operatoren (zie [query.md](../query.md#filtertaal)). CQL2-JSON over de lijn, CQL2-tekst in profielen en documentatie. Twee eigen functies: `actor()` (alleen in regels) en `text()` (vrije tekst op gemarkeerde velden). Paden over verwijzingen in `property`, maximaal 3 stappen.

## Alternatieven

- **CloudEvents-filterdialecten.** Simpel en bekend in de eventwereld, maar zonder vergelijkingen, tijd en ruimte.
- **Eigen JSON-grammatica (lab ADR-0003).** Moet alles zelf definiëren wat CQL2 al heeft.
- **OData `$filter`, RSQL, UCAST.** Minder uitdrukkingskracht of geen ruimte; geen gangbare JSON-vorm.

## Gevolgen

- Ruimte en tijd zijn standaard, met bestaande bibliotheken (de Rust-crate `cql2`, pygeofilter voor Python).
- CQL2-JSON is langer om te schrijven dan de korte vormen; daarom CQL2-tekst in profielen.
- Lab PR #53 schuift naar CQL2-JSON. Abonnementen volgen dezelfde taal, en wijken dus af van de CloudEvents Subscriptions API. Die afwijking staat in het lab-pattern.
