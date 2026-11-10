# ADR-0001: Eigen kern in Rust, lenen van ZaakChat en Atomic

- **Status:** Voorgesteld
- **Datum:** 2026-11-10

## Context

Er zijn drie vertrekpunten: ZaakChat (Rust, JSON Commit, SSE, projecties), atomic-server (Rust, schema's als data, KV-trait, plugins, websockets) of opnieuw beginnen.

## Beslissing

Een nieuwe, kleine kern in Rust, die code en ideeën overneemt waar dat past.

- **Rust**, voor snelheid, geheugengebruik en één binary.
- **Uit ZaakChat:** JSON Commit (`commits.rs`), idempotentie, `previousCommit`, SSE met snapshot en deltas, projecties en heads.
- **Uit atomic-server:** de vorm van de `KvStore`-trait, de index op omgekeerde verwijzingen, bewaakte query's.

## Alternatieven

- **ZaakChat uitbouwen.** Het domein zit erin vast: `involved` als enige autorisatie, issues en comments herkend aan velden, schema's als Rust-types, e-mail en push in de server. Uitpluizen kost meer dan de goede delen meenemen.
- **Op atomic-server bouwen.** Het model is anders: resources met Loro-CRDT-updates als commits, terwijl OpenEvents een log van CloudEvents is waaruit de stand volgt. Daarnaast ontbreken nu echt wissen (genesis-commits, envelopes en blobs blijven staan), strenge validatie bij binnenkomst en rollen. En een VNG-onderdeel op een product van één leverancier wekt de indruk van voortrekken.

## Gevolgen

- De opslaglaag houdt dezelfde vorm als in atomic-server. Zo kan OpenEvents later als crate of plugin in Atomic landen, of kan Atomic OpenEvents gebruiken voor zijn eventstroom.
- **Risico:** bijna alle leveranciers in het Common Ground-ecosysteem werken met Python of .NET. Dat maakt overdracht van beheer lastiger. Tegenmaatregelen: een container als product (niemand hoeft Rust te kennen om het te draaien), een conformance-testset zodat andere implementaties mogelijk zijn, en een contract dat volledig in het lab staat.
