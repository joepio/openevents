# ADR-0004: Autorisatie is een query

- **Status:** Voorgesteld
- **Datum:** 2026-11-10

## Context

Rechten zijn deels per deelnemer (inwoner, curator, bewindvoerder) en deels per organisatie (team, zaaktype, rol). Bestaande systemen filteren na de query (atomic-server, waardoor tellingen lekken), plakken tekst aan een query (ZaakChat, met risico op injectie), of halen alles op en filteren in het portaal (Open Inwoner). Lab issue #51 wil goede prestaties zonder controle per resource, filteren op velden, vertegenwoordiging, en aansluiting op bestaande standaarden.

Een eerste ontwerp liet een externe beslisser (AuthZEN) een filter teruggeven in het `context`-veld van zijn antwoord. Dat bleek te ingewikkeld: AuthZEN is gemaakt voor losse beslissingen, elke beslisser heeft een eigen vorm voor een filter, en je krijgt een tweede taal naast de query-taal.

## Beslissing

Een recht is een filter in dezelfde taal als `/query`, met `$actor` als variabele. Elk type in een profiel heeft regels voor `read`, `append` en `write`, en optioneel velden per rol. Relaties die voor rechten nodig zijn (teams, zaaktypen, lidmaatschap, deelnames) zijn data in het register. Een regel volgt daar een pad naar `$actor`.

De engine voert altijd `filter van de client AND leesregel` uit als één boom, vóór paginering, en past de veldregel toe op elke representatie en op de stroom.

## Alternatieven

- **AuthZEN-beslisser die een filter teruggeeft.** Rekt de standaard op, vraagt een adapter per beslisser, en geeft twee talen.
- **Centraal beleid dat naar registers wordt verspreid (OPA-bundles).** Nog steeds een tweede taal en een extra component.
- **ACL-lijsten per onderwerp.** Simpel voor deelnemers, maar een medewerker die op zaaktype toegang heeft, moet dan op elk onderwerp worden bijgeschreven. Bij een teamwissel moeten duizenden onderwerpen worden aangepast.
- **Controle per rij na de query.** Traag, en tellingen lekken.

## Gevolgen

- Eén taal en één engine voor bevragen, volgen en autoriseren. De autorisatie wordt even snel als de query, en is even goed getest.
- Uitlegbaar: "waarom zie ik dit?" is een query.
- Een organisatieprofiel (teams, leden, zaaktypen) hoort bij elke installatie. Bronnen zoals een IAM of FTV kunnen dat vullen via events.
- Lab issue #51 krijgt een voorstel dat zonder externe beslisser werkt. Het `context`-filtervoorstel van 3 november vervalt daarmee.
- Een centrale beslisser blijft mogelijk als optionele haak voor schrijfacties, niet voor lezen.
