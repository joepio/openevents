# ADR-0005: Wissen met crypto-shredding per onderwerp

- **Status:** Voorgesteld
- **Datum:** 2026-11-10

## Context

Een onveranderlijk log botst met de AVG en de Archiefwet. Lab ADR-0002 noemde tombstone plus crypto-shred als oplossing.

## Beslissing

Elk onderwerp heeft een eigen sleutel. De inhoud van events en projecties wordt daarmee versleuteld opgeslagen; de envelop blijft leesbaar. Wissen is: de sleutel vernietigen, projectie en indexen verwijderen, en een tombstone-event plaatsen. Sleutels hebben een kortere bewaartermijn in backups dan de data.

## Alternatieven

- **Events herschrijven.** Breekt cursors, hashes en eventuele handtekeningen.
- **Alleen dunne events met verwijzingen.** Veilig, maar dan heeft het register geen inhoud om op te bevragen, en verschuift het probleem naar de bron.

## Gevolgen

- Wissen werkt ook voor backups, na de bewaartermijn van sleutels.
- Metadata (type, tijd, onderwerp) blijft; profielen moeten daarom ondoorzichtige onderwerp-URI's gebruiken.
- Indexen op versleutelde velden moeten in dezelfde batch als de wis verdwijnen. Vrije-tekstindexen moeten per onderwerp te wissen zijn.
