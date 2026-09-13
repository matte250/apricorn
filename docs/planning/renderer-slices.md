# Renderer: små leveranser och verifieringskrav

Hör till [PLAN, fas 5B](../../PLAN.md). Reviderad 2026-09-12.
Alla implementationskort nedan är öppna. Befintlig kod är en utgångspunkt,
inte bevis på hårdvaruexakthet. Huvudkryssen finns i PLAN; här bockas delarna av.

Det isolerade implementationsutkastets omfattning och testresultat finns i
[renderer-pr-2026-09-13.md](renderer-pr-2026-09-13.md). Inga paritetskort
bockas av genom denna checkpoint.

## Körordning och gräns för varje PR

Börja med 5B.01a och 5B.09a: identifiera underlaget och isolera den saknade
markskuggan. Därefter följer 5B.09b och de separata rasteravvikelserna i 5B.04
och 5B.07. GX-state i 5B.03 måste verifieras före anspråk på full ljusparitet.
Korten 5B.05–08 levereras ett beteende åt gången, med egen före/efterrapport.

Varje implementations-PR innehåller en namngiven effekt, dess referens/prob,
minsta meningsfulla regression och råjämförelse. Den anger vilka befintliga
failures som kvarstår. Övriga lokala ändringar får inte följa med av misstag.
PR-rutinen och det uttryckliga målet `matte250/apricorn:main` finns i PLAN.

Gemensamt kontrakt för varje kort:

- Ange indata, state före/efter, registerbredd, signedness, mellanresultat,
  avrundningspunkt och overflowbeteende. Okända bitregler får en mätuppgift.
- Identifiera första avvikande steg: spelobjekt/asset → GX-kommandon →
  transformerad geometri → klippning → fragment → resolve → LCD-komposition.
  Ett fel i tidigare steg ska inte döljas med en korrigering i ett senare.
- Behåll samma ROM/save/RTC/input, observationstid och jämförelseprofil.
  Ingen gameplay-RNG får förbrukas av rendering eller cachemissar.
- Jämför färg inklusive alpha, djup, attribut och coverage separat. Upprepa
  med nästa bilds förändringsmask. Ändra inte goldens enbart för att få grönt.
- Exakt acceptans kräver noll avvikelser i angivna fall. Syntetiska tester,
  oraclelikhet och DS-hårdvarubevis redovisas var för sig. Oraclelikhet ensam
  bevisar inte att varje DS-hårdvarufall täcks.

## 5B.01 — underlag som går att återköra

- [ ] **5B.01a** Paketera genereringsrecept och manifest för befintlig sovrumsdiagnostik. Ange engine revision + dirty source digest, oracle build/patch, ROM/save/input/RTC, råformat och provpunkt. Saknat underlag ska ge ett namngivet fel. Historiska råfiler utan full startproveniens märks diagnostiska.
- [ ] **5B.01b** Spela in varje VBlank från deklarerad start, inklusive stillastående och upprepade bilder. Para enligt förbestämt tidskontrakt från 5A.04b. En förskjuten eller borttagen bild ska ge fel; ingen efterhandsvald offset.
- [ ] **5B.01c** Separata reproducerbara sekvenser för hus 1F och New Bark: stilla, gång i fyra riktningar, sväng, hinder och kartbyte. Fler avatarer/objekt läggs till med namngiven täckning.

Råformatets versionskontroll ska omfatta dimensioner, längd, endianordning och
reserverade fält. Dokumentera om capture sker före/efter resolve och vilka
attribut som faktiskt exporteras; jämför inte olika steg som samma yta.

## 5B.02–03 — numerik och GX-state före rasterisering

- [ ] **5B.02a** Inventera FX/SinCos-tabellens ursprung. Ersätt eller verifiera hostgenereringen mot originalets samtliga relevanta värden, inklusive wrap och negativa vinklar. Samma artefakthash på stödda byggen.
- [ ] **5B.02b** Kamera/model/projection/viewport: proba extrema koordinater, fraktionella kamerasteg, negativa mellanresultat och viewportgränser. Jämför clipkoordinater före division och skärmkoordinater efter kvantisering.
- [ ] **5B.03a** Dokumentera och testa kommandoavkodning, parameterantal och kvarstående vertexstate. VTX_16/10/XY/XZ/YZ/DIFF får gränsfall för signedness och wrap; ofullständig ström rapporteras med offset/opcode.
- [ ] **5B.03b** Verifiera när POLYGON_ATTR aktiveras samt BEGIN/END/strip-state. En materialändring mitt i en shape får inte tyst ersättas av ett enda material för hela meshen.
- [ ] **5B.03c** Position/vector/projection/texture-matriser: identity/load/multiply/scale/translate och stack push/pop/store/restore. Mät respektive stacks gränser, felstate och påverkan på ljusvektorer separat.

Ytor: `core/field/model.rs`, `gfx/field/camera.rs`, `gfx/field/mod.rs`,
`gfx/build.rs`. Behåll triangel-/quad-/stripidentitet genom exporten så att
senare rastersteg inte behöver gissa originalets polygoner.

## 5B.04 — clipping och culling

- [ ] **5B.04a** Culling före/efter projektion: front/back/båda/ingendera, speglad transform, windingbyte i strips, degenererade polygoner och W runt noll. Mät beslutspunkt och integerprecision.
- [ ] **5B.04b** Klipp ett plan åt gången: helt inne/ute, exakt på planet och en vertex på vardera sidan. Fastställ planets behandlingsordning, far-plane-bit och interpolering av position/färg/UV/W vid nya vertices.
- [ ] **5B.04c** Flera plan samtidigt, gemensam klippkant och polygoner som efter klippning har fler än fyra vertices. Kontrollera vertexordning, inga sprickor/dubbelfragment och bevarat polygon-ID.
- [ ] **5B.04d** Isolera avvikelsen i sovrumssekvensens övre bildrad med samma inmatade geometri i båda producenterna. Ändra raster eller clip först när det första felaktiga steget är identifierat.

Ytor: `gfx/field/mod.rs`, `gfx/gx3d.rs`. Ingen allmän triangulering av quads
utan bevis att interpolation och coverage bevaras i det verifierade fallet.

## 5B.05 — normals, ljus, toon och shininess

- [ ] **5B.05a** Mät NORMAL:s transformation och när beräknad vertexfärg låses. Sekvenser med COLOR/NORMAL/materialändring i olika ordning ska bevara föregående state där originalet gör det.
- [ ] **5B.05b** Ett ljus: emission/ambient/diffuse separat, avstängt ljus, negativt/skuggat dotproduktfall, saturation och normalgränser. Jämför mellansteg och rå vertexfärg.
- [ ] **5B.05c** Fyra ljus och deras mask/ordning. Byt LIGHT_VECTOR och matris i olika ordning, därefter ljusfärg/material utan nytt NORMAL. Ingen implicit omberäkning för hela meshen.
- [ ] **5B.05d** Specular först utan tabell, sedan SHININESS: alla tabellpositioner, gränser och enable-bit. Tabelländring mitt i strömmen ska få effekt vid rätt kommando.
- [ ] **5B.05e** Toon/highlight: indexgränser, tabelluppdatering, texturerad/otexturerad polygon och kanaloverflow. Verifiera render mode och display flag ihop.
- [ ] **5B.05f** Koppla mätta regler till områdets och dygnets ROM-ljus. Verifiera före/efter ljusbyte på samma tidskälla som simulationen.

Ytor: materialparser, kommandostate och `light_vertex`/fragmentfärg. Spara
vertexfärg från geometry capture så att ljusfel skiljs från textur-/blendfel.

## 5B.06 — texturer och alpha

- [ ] **5B.06a** Befintliga palettformat samt A3I5/A5I3: palettbas, index noll, alla alphavärden, negativa UV, clamp/repeat/flip och texelgränser.
- [ ] **5B.06b** DIRECT: kanal-/alphabitar, gränstexlar och genomskinlighet genom hela fragmentvägen. Ostött format ska fram till dess ge ett uttryckligt fel.
- [ ] **5B.06c** COMP4x4: blockadress, descriptor/palettadress och varje kompressionsläge. Verifiera avrundning och transparens med isolerade block före en riktig karta.
- [ ] **5B.06d** Texture-matrix modes för UV/normal/vertex, med tidigare state bevarat över kommandon. Testa modulation/decal, alpha-test och wireframe i separata fall.

Ytor: `core/field/model.rs` texturdata och `gfx/gx3d.rs` sampling. Behåll
nödvändiga rådescriptorer; expanderad RGBA får inte kasta bort information som
krävs för native precision. Cacheinvaliditet vid ändrad palett måste specificeras.

## 5B.07 — djup och translucent lager

- [ ] **5B.07a** Z-interpolation: horisontell/vertikal och flack kant, olika W, lika endpoints och stora djupskillnader. Isolera de fyra felaktiga djupen vid x=43–46 i sovrumsdiagnostiken innan generell ändring.
- [ ] **5B.07b** W-buffer: normalisering per polygon, precisionströsklar, rekonstruerat djup och clear/equal-test. Proba intill varje uppmätt gräns; ingen generell Z→W-konvertering antas.
- [ ] **5B.07c** Djup less/equal, opaque/translucent-ID, återanvänt ID och depth-write av/på. Testa två överlappande polygoner i båda ordningar samt alpha-test som avvisar fragmentet utan sidoeffekt.
- [ ] **5B.07d** AA:s övre/undre lager: överlapp, delvis täckning, clearbakgrund och translucent fragment mellan lagren. Verifiera ID/djup/fogstate för valt lager, även när det andra lagrets djup misslyckas.

Ytor: `gfx/gx3d.rs`. Clearvärden, sortering och renderregister ingår i
fixtureidentiteten. Ett korrekt slutligt RGB kan dölja felaktigt depth/ID.

## 5B.08 — fyra separata effekter, därefter kombinationer

- [ ] **5B.08a** GX shadow mode: maskpolygon ID 0 och efterföljande skuggpolygoner, depth fail/pass, självskuggning, masklivslängd och ID-regler. Testa övre/undre lager separat. Detta är ett annat flöde än den texturerade markskuggan i 5B.09.
- [ ] **5B.08b** Fog utan AA: enable per polygon/globalt, tabellinterpolation, offset/shift, ändpunkter, Z/W och color/alpha-läge. Koppla riktig ROM-tabell där den används.
- [ ] **5B.08c** Edge marking utan fog: ID-grupper, djupjämförelse, grannar, viewportkanter och clear-ID. Verifiera edgefärg/alpha/coverage och ordning relativt fog/AA.
- [ ] **5B.08d** AA isolerat: vertikala/horisontella/flacka kanter, båda lutningar, enpixelpolygoner och klippta spans. Kontrollera coverage även efter avvisade depth-/alpha-fragment.
- [ ] **5B.08e** Isolera pixeln (71,120) i sovrumsbild 0 med två lager före resolve; lika djup/attribut och olika slutfärg räcker inte för att fastställa orsaken.
- [ ] **5B.08f** Kombinera fog + edge + AA + translucens/skuggor i riktade fall. Verifiera hela resolveordningen och minst en riktig scen per faktiskt använd kombination.

Ytor: `Gx3dFrame`, lagerbuffertar, rasterisering och resolve. Hårdkodade
fältregister ersätts först när deras riktiga källa/livslängd är kartlagd.

## 5B.09 — saknad geometri och animation

- [ ] **5B.09a** Identifiera markskuggans ROM-asset, originalets submissionfunktion och synlighetsvillkor. Matcha dumpens polygoner till källan; mät position/höjd/skalning/alpha och polygonordning utan skärmkoordinathack.
- [ ] **5B.09b** Koppla markskuggan genom objektstate och render submission. Prova stilla/gång/sväng/dölj/warp samt höjd/terräng där originalet skiljer sig. Skuggdata läses från ROM vid körning.
- [ ] **5B.09c** Inventera saknade animerade meshes och Nitro-animationer; vindkvarnsblad blir första separat namngivna konsument om inventeringen bekräftar det. Mät frameval/loop/synlighet före implementation.
- [ ] **5B.09d** Objekt-offsets och billboard clipping: kamera-/världsrum, djupbias, spegling, storlekar och delvis utanför viewport. Jämför geometri före raster och rörelse över flera bilder.

Ytor: `core/field/map_object.rs`, `field/system.rs`, `gfx/field/billboard.rs`
och render submission. Samma objektidentitet ska användas för kropp/skugga;
cache eller renderpass får inte driva dess animationstimer.

## 5B.10 och delmilstolpen

- [ ] **5B.10a** Releaseprofil för simulation, assetladdning, geometri, raster och resolve med namngiven maskin och kalla/varma mätningar.
- [ ] **5B.10b** Lägg till LCD-komposition, upload/presentation och backlog; rapportera p50/p95/p99/max samt minne. Rawrenderns tidsgräns räcker inte som slutbudget.
- [ ] **5B.Ga** Granska själv konsekutiva bilder vid stolar/bänk/Moms bord och ute under rörelse. Dokumentera faktiskt granskad sekvens och kvarvarande artefakter.
- [ ] **5B.Gb** Noll rawdiff i deklarerad matris och användarens visuella regression. Märk generell GX-/hårdvarutäckning separat och stäng inte huvudgaten medan dess öppna kort återstår.

## Körd diagnostik 2026-09-12

Körd på arbetsgrenen `codex/renderer-parity`, HEAD
`b15334c7804c0777b14e43a65e9685c666d21e1d` **med befintliga lokala kodändringar**.
Detta är inte ett resultat för den rena commiten eller plan-PR:ens kod.
Rust/Cargo 1.98.1 på Windows, MinGW i PATH. ROM SHA-1 är profilens
`4fcded0e2713dc03929845de631d0932ea2b5a37`.
pret: `0985e8718df4f25e64d6507d89c0c97c0d288981`;
melonDS-checkout: `b86390e4428bf38ce4c1ce0e9ca446d6d25955e8`.
Det befintliga oraclebyggets fulla koppling till historiska captures har inte
återskapats; därför förblir 5B.01a öppen.

Lokalt under `out/renderer-parity-20260912/` finns source-manifest med SHA-256
per kodfil, engine raw/PNG, `pairs.tsv`, `field-test.log` och `raw-diff.log`.
ROM/save/raw publiceras inte. Recept från reporoten, med fungerande toolchain:

```powershell
$env:APRICORN_RENDER_OUT = Join-Path (Get-Location) 'out/renderer-parity-20260912'
cargo test -p apricorn-gfx --test field_system_hg bedroom_walk_has_a_pinned_contiguous_raster_sequence -- --nocapture
cargo run -p apricorn-harness --bin apricorn-gx-diff -- --sequence out/renderer-parity-20260912/pairs.tsv
```

Testet skriver bilder före sin goldenassertion och **misslyckas** mot det gamla
facit. Råjämförelsen **misslyckas** också, med följande antal avvikande pixlar:

| Enginebild | Oraclebild | Färg | Djup | Attribut | Coverage | Förändringsmask |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 0 | 4822 | 29 | 0 | 28 | 0 | — |
| 1 | 4822 | 29 | 0 | 28 | 0 | 0 |
| 2 | 4823 | 29 | 0 | 28 | 0 | 0 |
| 3 | 4825 | 29 | 0 | 28 | 0 | 0 |
| 4 | 4827 | 29 | 0 | 28 | 0 | 0 |
| 5 | 4829 | 39 | 4 | 38 | 0 | 0 |
| 6 | 4831 | 40 | 0 | 39 | 0 | 0 |
| 7 | 4833 | 39 | 4 | 38 | 0 | 0 |
| 8 | 4835 | 40 | 0 | 39 | 0 | 0 |
| 9 | 4837 | 63 | 5 | 29 | 0 | 1 |

`pairs.tsv` använder samma historiska urval som `oracle/sequences/bedroom-down.tsv`,
men pekar på nygenererade enginefiler. Full VBlank-paritet är inte testad.
För att återskapa manifestet behövs lokala captures
`out/oracle-gx-motion-down/frame_NNNNNN_gx3d.bin` enligt tabellen; enginefilerna
heter `field-motion-NN.gx3d.bin` under renderutmatningen. En rad innehåller
oracle- och engineväg separerade med tab, relativt manifestets katalog.

Observationer, med nollbaserade pixelkoordinater:

- Bild 0: 28 attributavvikelser kring spelarens fötter, x=120–134/y=96–99.
  Oracle har translucent-ID 2, engine saknar det. Geometridumpen för 4822
  innehåller fyra texturerade polygoner 292–295 med ID 2 och alpha 14;
  enginevägen skickar meshes och billboards utan markskugga. Det stödjer
  hypotesen saknad submission; ROM-asset och originalfunktion återstår.
- Bild 0: pixel (71,120) har lika djup/attribut men olika färg. Undre lager och
  resolve är nästa isolering, inte en bevisad rotorsak.
- Bild 5 och 7: fyra djupavvikelser vid x=43–46. Bild 9 har dessa plus ytterligare
  ett djupfel och ett färgspann vid övre bildraden. Klippning och kantinterpolation
  utreds separat från markskuggan.

Ingen rendererimplementation eller goldenändring ingår i denna dokumentleverans.
Övriga sceners tester/visuell acceptans och full testsvit har inte körts som
del av denna riktade diagnostik. Ingen fullständig fas bockas av.

## Lästa tekniska underlag

[ndsdoc: vertex/polygon commands](https://ndsdoc.gbadev.net/3d_vertex_polygon_commands.html)
beskriver kvarstående vertexstate, POLYGON_ATTR vid nästa vertexlista och NORMAL
som färgberäkning med dåvarande ljusstate. Det motiverar separata kommandoprober
i 5B.03/05; ett enda slutmaterial per mesh kan inte användas som generellt bevis.
[BlocksDS: advanced 3D](https://blocksds.skylyrac.net/tutorial/advanced/advanced_3d/)
ger ett körbart upplägg för stencilbaserade GX-skuggor. Det används som underlag
för ett isolerat testfall, inte som facit för HeartGolds texturerade markskugga.
Dokumentationens exakta aritmetik/gränsfall måste verifieras med prober.
Projektens kod kopieras inte; licensgränsen i PLAN gäller även här.
