# ADR-0004: Autorisatie als filter in de query-boom

- **Status:** Voorgesteld
- **Datum:** 2026-11-10

## Context

Rechten zijn deels per onderwerp (deelnemers aan een gesprek, curator, bewindvoerder) en deels regels (rol, zaaktype). Bestaande systemen filteren vaak na de query (atomic-server, waardoor tellingen lekken) of plakken tekst aan een query (ZaakChat, met risico op injectie). Open Inwoner haalt alle taken op en filtert in het portaal.

## Beslissing

- Rechten per onderwerp staan in een ACL, bijgehouden door de projectieregels van het profiel, en als index (`idx_readable`).
- Regels komen van een externe beslisser via AuthZEN. Het antwoord bevat een filter en een veldenlijst (lab issue #51).
- De engine bouwt `queryFilter AND (aclFilter OR beslisserFilter)` op de geparste boom en voert dat uit vóór paginering. Velden worden gemaskeerd in elke representatie en op de stroom.
- Schrijven: ACL eerst, beslisser als het beleid dat zegt; een weigering is één 403 zonder details.

## Gevolgen

- Tellingen en cursors lekken niets.
- Het filter van de beslisser gaat door dezelfde parser en grenzen als een clientquery.
- AuthZEN kent nog geen standaardvorm voor een filter in het antwoord. Dat is een voorstel dat we via het lab willen inbrengen.
