# ADR-0007: Domeinlogica in WASM-modules naast het register

- **Status:** Voorgesteld
- **Datum:** 2026-11-10

## Context

OpenEvents is de leeskant (CQRS). Domeinlogica moet ergens anders: wat mag bij een bezwaar, wanneer verloopt een termijn, wanneer is een melding een dubbele. Zaaktypen in BPMN zijn daarvoor niet krachtig genoeg en verliezen van specialistische pakketten. Specialistische pakketten zijn echter silo's.

## Beslissing

Domeinlogica komt in kleine, open modules: een profiel plus WASM-componenten voor commands, reacties en timers, plus formulieren en tests. Een modulehost naast OpenEvents voert ze uit in een sandbox, met een vaste set host-functies (`stand`, `query` met de rechten van de gebruiker, `nu`, `fetch` naar een allowlist, `log`). Zie [modules.md](../modules.md).

## Alternatieven

- **Logica in het register (projectie-plugins).** Mengt C en Q, maakt het register zwaarder en afspelen van het log onvoorspelbaar.
- **BPMN-engine.** Goede orkestratie, maar het domein zit er niet in. Mag binnen een module.
- **Microservices per domein in eigen containers.** Kan altijd, want een module is ook gewoon een client van OpenEvents. Maar dan is er geen sandbox, geen vaste interface, geen gedeelde tests en is delen tussen gemeenten zwaarder.

## Gevolgen

- Modules zijn bijna pure functies: toetsbaar met scenario's, deelbaar met versies, veilig door de sandbox.
- Elke taal die naar WASM-componenten compileert, kan; JavaScript via QuickJS verlaagt de drempel voor leveranciers.
- De runtime en de ervaring uit atomic-server (wasmtime, QuickJS, fuel-limieten, geheimen die de host invult) zijn te hergebruiken.
- Een module is ook als gewone service te draaien voor wie geen WASM wil. Het contract (commands, events, tests) is hetzelfde.
