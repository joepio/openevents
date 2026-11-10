# ADR-0002: Opslag op een key-value-store achter een trait

- **Status:** Voorgesteld
- **Datum:** 2026-11-10

## Context

Een eventregister heeft een paar vaste toegangspatronen: aanvullen op volgorde, opvragen per sleutel, bereiken scannen (log vanaf een cursor, events per onderwerp, indexen). Daarnaast zijn de eisen duurzaamheid (K1), online backup (K2), failover (K3) en eenvoudig hosten (K6).

## Beslissing

De kern praat alleen met een trait met `get`, `insert`, `remove`, `scan_prefix`, `range` en een atomaire `apply_batch`. Indexen en query's zijn code in de kern, niet in de store.

De eerste implementatie is embedded, met synchroon schrijven bij bevestiging. Een tweede implementatie op een gedeelde store komt zodra failover nodig is. Welke stores dat worden, beslissen we op de eisen, niet vooraf.

## Alternatieven

- **Direct een relationele database.** Werkt, maar dan zit de query-engine half in SQL en half in Rust, en wordt eenvoudig hosten lastiger.
- **Een bestaande eventstore of message broker.** Die geven log en stroom, maar geen projecties, indexen, autorisatie op onderwerp of wissen.

## Gevolgen

- Volledige vrijheid over indexen en de query-planner.
- **Let op:** atomic-server schrijft met een flush-tick van 100 ms en kan bevestigde writes verliezen. Dat nemen we niet over: bevestigen betekent gesynchroniseerd.
- Backup en herstel naar een tijdstip moeten we zelf maken (bijvoorbeeld een replica die het log volgt). Dat zit in fase 4 van de roadmap.
