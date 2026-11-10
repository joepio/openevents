# Wissen en bewaren

Een eventlog is onveranderlijk, maar de AVG (recht op verwijdering) en de Archiefwet (vernietigen na de bewaartermijn) vragen dat inhoud echt kan verdwijnen. Lab ADR-0002 schrapte event sourcing als publiek contract juist om die reden. OpenEvents lost het op in de opslag, zodat het log zijn voordelen houdt.

## Crypto-shredding per onderwerp

- Elk onderwerp krijgt bij het eerste event een eigen sleutel in de tabel `keys`.
- `data` van elk event en de projectie van het onderwerp worden met die sleutel versleuteld opgeslagen. De envelop (`id`, `type`, `subject`, `time`, `sequence`) blijft leesbaar, zodat volgorde en cursors blijven werken.
- Wissen is: de sleutel vernietigen, de projectie en de indexrijen verwijderen, en een tombstone-event `nl.vng.openevents.subject.erased` op het log zetten.
- Backups bevatten alleen versleutelde inhoud. Sleutels staan apart, met een eigen, korte bewaartermijn voor backups. Zo is een wis na die termijn ook uit backups niet meer te herstellen (eis W1).

Wat overblijft na een wis: dat er events waren, van welk type en wanneer. Als ook dat te veel zegt (bijvoorbeeld bij een `subject`-URI met betekenis), dan moet het profiel ondoorzichtige onderwerp-URI's gebruiken. Dat is een regel in de profielrichtlijn.

## Bewaartermijnen

- Een onderwerp kan een `retainUntil` krijgen. Bij Gesprekken volgt die het bovenliggende object (de zaak), zoals Vincent op 5 november bevestigde.
- Een interne taak wist onderwerpen waarvan de datum verstreken is, op dezelfde manier als een wisverzoek.
- Archiveren (overbrengen naar een e-depot) is een consumer: die leest de tijdlijn vóór de vernietiging en schrijft een bewijs terug als event.

## Abonnees

Het tombstone-event gaat over de stroom zoals elk ander event. Consumers die een kopie hebben (een portaal-cache, een zoekindex) moeten daarop hun kopie wissen. Dat is onderdeel van de conformance-tests voor consumers.

## Gevoelige velden

Gevoelige velden (BSN, adres) horen niet in events als het ook met een verwijzing kan: een deelname verwijst naar een Partij in Open Klant. Waar een gevoelig veld toch nodig is, markeert het profiel het als `sensitive`. Zo'n veld wordt standaard gemaskeerd op de stroom en alleen getoond aan wie het expliciet in `fields` vraagt en er recht op heeft.
