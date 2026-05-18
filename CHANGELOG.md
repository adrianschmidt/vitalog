# [1.4.0](https://github.com/adrianschmidt/vitalog/compare/v1.3.2...v1.4.0) (2026-05-18)


### Features

* **config:** honor $VITALOG_CONFIG for sandbox configs ([3977439](https://github.com/adrianschmidt/vitalog/commit/39774394647d68a9963ed24ff4118f842e040647))

## [1.3.2](https://github.com/adrianschmidt/vitalog/compare/v1.3.1...v1.3.2) (2026-05-17)


### Bug Fixes

* **today:** suppress duplicate BP custom-metric rows + add evening composite ([896dd8e](https://github.com/adrianschmidt/vitalog/commit/896dd8ee7b81085ef43c9420c111b180ba98ff64))

## [1.3.1](https://github.com/adrianschmidt/vitalog/compare/v1.3.0...v1.3.1) (2026-05-16)


### Bug Fixes

* **bp:** classify pre-day_start_hour readings as evening ([#34](https://github.com/adrianschmidt/vitalog/issues/34)) ([8c89928](https://github.com/adrianschmidt/vitalog/commit/8c8992859e3406d2f7d068797adbab7d4fa8cf35))

# [1.3.0](https://github.com/adrianschmidt/vitalog/compare/v1.2.0...v1.3.0) (2026-05-13)


### Features

* **reminders:** apply time gate inside `evaluate` ([34a4db7](https://github.com/adrianschmidt/vitalog/commit/34a4db713e200cdf4b4f98d70771da5a3eff7ca0))
* **reminders:** expose not_before / not_after in JSON output ([f087ded](https://github.com/adrianschmidt/vitalog/commit/f087ded23271985a18b23dd4981d7592bdafc6cf))
* **reminders:** parse + validate not_before / not_after ([21da846](https://github.com/adrianschmidt/vitalog/commit/21da846a62db1c68dd7cc7f4bdd91769b813375f))
* **reminders:** scaffold not_before / not_after fields on types ([f718829](https://github.com/adrianschmidt/vitalog/commit/f718829685eb9d8210dc247f3db2a38a41eff824))
* **reminders:** within_time_window helper with day_start_hour awareness ([a34a33f](https://github.com/adrianschmidt/vitalog/commit/a34a33f5317f23dda99d7c21f45b35701cdb7a7f))

# [1.2.0](https://github.com/adrianschmidt/vitalog/compare/v1.1.0...v1.2.0) (2026-05-12)


### Features

* **reminders:** evaluate day_field watches; exhaustive match ([e086c2b](https://github.com/adrianschmidt/vitalog/commit/e086c2b296c190cfb68b6dbb3f33e9192f1b34a1))
* **reminders:** evaluate lift watches with optional min filters ([2140afa](https://github.com/adrianschmidt/vitalog/commit/2140afab706cbb12ec326717512ea3d410d0c19a))
* **reminders:** evaluate metric watches against the DB ([f363c38](https://github.com/adrianschmidt/vitalog/commit/f363c382338d8b07e4fc5aeecb52975fee6e2ec6))
* **reminders:** evaluate session watches ([04ae65b](https://github.com/adrianschmidt/vitalog/commit/04ae65bd643b5ef3a7f1966ad51e7f9c78bd9f9d))
* **reminders:** include reminders + reminder_warnings in status JSON ([feb4f0f](https://github.com/adrianschmidt/vitalog/commit/feb4f0fbe1388ec68f89f7a315368657b21bf551))
* **reminders:** parse [reminders] config into Reminder values ([887e5d9](https://github.com/adrianschmidt/vitalog/commit/887e5d937841d611c586303fd3df96cba2b0ba13))
* **reminders:** prepend due reminders block in `vitalog today` text ([3750ae5](https://github.com/adrianschmidt/vitalog/commit/3750ae5fbe77cc07b9106fe359726826eb743e2d))
* **reminders:** reminders array + reminder_warnings in today --json ([398bf2f](https://github.com/adrianschmidt/vitalog/commit/398bf2f2ca926d5c5261024b57b99f60f2ba3a0c))
* **reminders:** scaffold reminders module + Config field ([f647ff8](https://github.com/adrianschmidt/vitalog/commit/f647ff8d6cfd686704103c0fea1bf55826d29ef4))

# [1.1.0](https://github.com/adrianschmidt/vitalog/compare/v1.0.4...v1.1.0) (2026-05-07)


### Bug Fixes

* **trend:** assert OLS denominator invariant; tighten empty-stats test ([#9](https://github.com/adrianschmidt/vitalog/issues/9)) ([d45761e](https://github.com/adrianschmidt/vitalog/commit/d45761ee6ed396903a20080dfcacd59151ed28ad))
* **trend:** match today's sync error policy; ensure date labels separated ([#9](https://github.com/adrianschmidt/vitalog/issues/9)) ([faaa7bb](https://github.com/adrianschmidt/vitalog/commit/faaa7bb468d6d411b0a24a2ca541a721ff6cb616))


### Features

* **trend:** add types and compute_stats with OLS slope ([#9](https://github.com/adrianschmidt/vitalog/issues/9)) ([fd46ae1](https://github.com/adrianschmidt/vitalog/commit/fd46ae169792003ba633364a5ff8a0659b507122))
* **trend:** build_window expands DB rows to full date range ([#9](https://github.com/adrianschmidt/vitalog/issues/9)) ([157ddac](https://github.com/adrianschmidt/vitalog/commit/157ddac515bd8ba66fd58676f3c9bb562f440730))
* **trend:** render_chart with multi-row ASCII output ([#9](https://github.com/adrianschmidt/vitalog/issues/9)) ([65f011c](https://github.com/adrianschmidt/vitalog/commit/65f011c5f1b03f80be0952ad3ed1f49309e89bbd))
* **trend:** render_compact with sparkline blocks ([#9](https://github.com/adrianschmidt/vitalog/issues/9)) ([fff7d4c](https://github.com/adrianschmidt/vitalog/commit/fff7d4c22e9ae452bd319ce418576b214f666484))
* **trend:** render_json with full window and stats ([#9](https://github.com/adrianschmidt/vitalog/issues/9)) ([c9162a8](https://github.com/adrianschmidt/vitalog/commit/c9162a8193d45896edd2ee433acaa98df4f80495))
* **trend:** resolve_field with builtins and metric fallback ([#9](https://github.com/adrianschmidt/vitalog/issues/9)) ([598ca96](https://github.com/adrianschmidt/vitalog/commit/598ca9637429ef0fae388c170da2020a7c2fe412))
* **trend:** scaffold CLI subcommand and dispatch ([#9](https://github.com/adrianschmidt/vitalog/issues/9)) ([487cdc6](https://github.com/adrianschmidt/vitalog/commit/487cdc69c80b0bc889c82dcdc7154d07292ef9cf))
* **trend:** TrendData + assemble entry point ([#9](https://github.com/adrianschmidt/vitalog/issues/9)) ([f2d8f0e](https://github.com/adrianschmidt/vitalog/commit/f2d8f0eebffc462480e295816c3adb715529fbd8))
* **trend:** wire execute end-to-end with sync-on-run ([#9](https://github.com/adrianschmidt/vitalog/issues/9)) ([1f9f419](https://github.com/adrianschmidt/vitalog/commit/1f9f4193a22d104d2f2607f14d42b99707323d64))

## [1.0.4](https://github.com/adrianschmidt/vitalog/compare/v1.0.3...v1.0.4) (2026-05-07)


### Bug Fixes

* **today:** annotate weight row over/under threshold like food rows ([b5f68a9](https://github.com/adrianschmidt/vitalog/commit/b5f68a93275e081bab9a87c5a388e7b9ca60d2d3)), closes [#18](https://github.com/adrianschmidt/vitalog/issues/18)

## [1.0.3](https://github.com/adrianschmidt/vitalog/compare/v1.0.2...v1.0.3) (2026-05-07)


### Bug Fixes

* **today:** sync notes before reading summary ([af0d3ba](https://github.com/adrianschmidt/vitalog/commit/af0d3ba6d55fe2a82aaa47912568db425c1ff151))

## [1.0.2](https://github.com/adrianschmidt/vitalog/compare/v1.0.1...v1.0.2) (2026-05-07)


### Bug Fixes

* **today:** render target alongside min/max in text output ([f1adc7e](https://github.com/adrianschmidt/vitalog/commit/f1adc7ec3a69c6700c265c4f6d912a7f177c5e41)), closes [#19](https://github.com/adrianschmidt/vitalog/issues/19)

## [1.0.1](https://github.com/adrianschmidt/vitalog/compare/v1.0.0...v1.0.1) (2026-05-07)


### Bug Fixes

* **preset:** rename remaining daylog references to vitalog ([cd5648a](https://github.com/adrianschmidt/vitalog/commit/cd5648afca5f7d586eb1a75fc50a9e2b96ccc9b8))

# 1.0.0 (2026-05-05)


### Bug Fixes

* allow MPL-2.0 license, ignore paste unmaintained advisory ([374f9a2](https://github.com/adrianschmidt/vitalog/commit/374f9a28ee688e5bdc0d714b3697ea21f4ee987b))
* cargo-deny v2 config syntax ([529ee0a](https://github.com/adrianschmidt/vitalog/commit/529ee0a41f418edee70634633f2de382b6f3f588))
* case-insensitive dedup + consolidate use statements ([eed5df8](https://github.com/adrianschmidt/vitalog/commit/eed5df89d87d0d1e9911f63c679bbdde299669e7))
* collapse sleep validation into match guard ([6a4e3b7](https://github.com/adrianschmidt/vitalog/commit/6a4e3b7cc5bedb1e1cf73c90440b199980dfb944))
* food_cmd applies --gi/--gl/--ii overrides in lookup mode ([0c70702](https://github.com/adrianschmidt/vitalog/commit/0c70702f2cfe1b0fb4d974905023b53326a50c64))
* food_cmd GL lookup uses chosen panel, not input unit ([e767734](https://github.com/adrianschmidt/vitalog/commit/e767734f21d7b5f7c484690c152d6eeaf040fdce))
* **goals:** formatting + CRLF tolerance + case-sensitivity note ([d571c96](https://github.com/adrianschmidt/vitalog/commit/d571c9687b5f9d4742c291c16b6b1c9d43c445c4))
* input validation, integration tests, cleanup for launch ([941d6a6](https://github.com/adrianschmidt/vitalog/commit/941d6a61421252e26aa2911377aadde94587c82d))
* normalise empty-string last_nutrition_sync to None ([49dd430](https://github.com/adrianschmidt/vitalog/commit/49dd430885ec3e4bcb5d2f9fa58cc4dfd84bba7a))
* render weight_unit in daily-note template comment ([f1ab163](https://github.com/adrianschmidt/vitalog/commit/f1ab163d666f8fbb2dd8faf0331b6f059a71d4a0)), closes [#11](https://github.com/adrianschmidt/vitalog/issues/11)
* sleep_quality routing, metric validation, watcher reconnect ([3fa9a15](https://github.com/adrianschmidt/vitalog/commit/3fa9a157124885014bf6b5c8336d01be4ee5f426))
* surface real DB errors from nutrition_status ([d3ada7b](https://github.com/adrianschmidt/vitalog/commit/d3ada7be11d5a456f2e5aa7e7c8d3b6d3ed01413))
* **today:** clamp trim_num precision to one decimal ([78aace2](https://github.com/adrianschmidt/vitalog/commit/78aace2847f0eb87ddeb6d1ac3282ed5803b86b3))
* **today:** propagate I/O errors + tighten weight-delta loop + suggestion ([39fe235](https://github.com/adrianschmidt/vitalog/commit/39fe23508685dc1f6691adde4c11efbaaabbb7e2))
* Windows test failures from backslash paths in TOML ([7da29ca](https://github.com/adrianschmidt/vitalog/commit/7da29cabb2b14d9b24f02b033edeece7dae6f5ec))


### Features

* add `daylog today [date]` CLI skeleton ([43ed9c4](https://github.com/adrianschmidt/vitalog/commit/43ed9c449ec0c97e6f88c32d0c75a678aa4d911e))
* add cmd_sleep_end with stale-pending guard ([f660992](https://github.com/adrianschmidt/vitalog/commit/f660992ea79df3c4ef30c3bc107fcb22524a7cd7))
* add cmd_sleep_start ([6c6a245](https://github.com/adrianschmidt/vitalog/commit/6c6a2450cdd2ce5ba7db7790dbc94887841b9d5e))
* add configurable day boundary via day_start_hour ([e3c2408](https://github.com/adrianschmidt/vitalog/commit/e3c2408b0064e96eedc3799cabf7f9f06e36d1bc))
* add configurable weight_unit (lbs/kg) to config ([a4a4d70](https://github.com/adrianschmidt/vitalog/commit/a4a4d705790bd6a2bc44ee1713d5865c2094c4f0))
* add FoodInsert/FoodLookup types and CRUD helpers ([8958b0e](https://github.com/adrianschmidt/vitalog/commit/8958b0e7990f468f21fec193fecb74210de1736c))
* add foods, food_aliases, food_ingredients schema ([d941e7a](https://github.com/adrianschmidt/vitalog/commit/d941e7a8ce70a6068b9cdbd311d16506086145a6))
* add format_time helper to time module ([b6129a5](https://github.com/adrianschmidt/vitalog/commit/b6129a5e6843332fdff468fe083bfd83081ac16c))
* add nutrition_status helper for status JSON ([a054a55](https://github.com/adrianschmidt/vitalog/commit/a054a55c6faf16badc506a04a83cad7a162a2ef2))
* add sleep range parsing/formatting and hours math ([e019db7](https://github.com/adrianschmidt/vitalog/commit/e019db7d7799d6150248ec35ead820abd7d2ebe1))
* add state module for pending sleep-start sidecar ([c6e148f](https://github.com/adrianschmidt/vitalog/commit/c6e148f811a1abfbd9c8876138944dea12bad775))
* add time module with parse_time (12h + 24h) ([ac4389d](https://github.com/adrianschmidt/vitalog/commit/ac4389d99d75bf266c04f8d6fbc210b854a003b6))
* add time_format config option (12h/24h, default 12h) ([ad43842](https://github.com/adrianschmidt/vitalog/commit/ad43842f3075431fdb65aaede4267770c0418d55))
* address 6-agent review feedback for sleep commands ([a99da75](https://github.com/adrianschmidt/vitalog/commit/a99da75858156f84d31f5af738713a5a4cdaa8ef))
* assemble DaySummary from notes + DB and wire execute() ([df0a1ab](https://github.com/adrianschmidt/vitalog/commit/df0a1abffa898c80addab7ea12b81652e499aace))
* body::append_line_to_section preserves blank-line tail ([18f8ecb](https://github.com/adrianschmidt/vitalog/commit/18f8ecb524683fbbaa31e058fb0aa4f6442ac64f))
* body::ensure_section for canonical-order section insertion ([8838ae4](https://github.com/adrianschmidt/vitalog/commit/8838ae436282598f960ab5edd08caa5ba19429ac))
* build FoodInsert from a parsed entry with validation ([11b43b0](https://github.com/adrianschmidt/vitalog/commit/11b43b07f1e5afda778600020db4ce74b07b53b3))
* **cli:** add vitalog migrate command ([8d395da](https://github.com/adrianschmidt/vitalog/commit/8d395dac28e16a76ff0154b0244f9c4a5db9fd23))
* **cli:** show full logged line + daily totals on food/note/bp ([844b0a6](https://github.com/adrianschmidt/vitalog/commit/844b0a6ad0f45792df78d8c39f3e394ba99a870f))
* **config:** fall back to legacy daylog paths with one-time stderr hint ([8a66452](https://github.com/adrianschmidt/vitalog/commit/8a66452c542809102970f02dae1cbff91488acdf))
* daily-note template includes Food and Vitals sections ([035aaad](https://github.com/adrianschmidt/vitalog/commit/035aaad563a681127bb35cbe44708cdf14b2dd04))
* dashboard formats sleep per time_format config ([77d4384](https://github.com/adrianschmidt/vitalog/commit/77d4384f7e1cb25ab267bd7ddfcedb74243277a6))
* daylog bp writes YAML and Vitals body in one atomic pass ([f06b97a](https://github.com/adrianschmidt/vitalog/commit/f06b97aa70aef96d9af636c149992a91a9497339))
* daylog food handler — DB lookup, custom flags, file write ([d8742ac](https://github.com/adrianschmidt/vitalog/commit/d8742acbc14ed0ad676ec46bf1e3a54e2f6c55f1))
* daylog note appends to ## Notes with alias and date/time flags ([b50d98a](https://github.com/adrianschmidt/vitalog/commit/b50d98ae4e1378a04dc03f70179d81b9669bf3ac))
* daylog readme subcommand prints embedded README ([bf2f558](https://github.com/adrianschmidt/vitalog/commit/bf2f55830d37f0fdc367407e894a1ecedabc14ec))
* **daylog:** terminal dashboard that tracks your life from markdown notes ([b510b17](https://github.com/adrianschmidt/vitalog/commit/b510b17b4e06d7e386b0a3fb0dd41f662c2ba7ec))
* food_cmd amount parser with g/ml suffix ([3678dc8](https://github.com/adrianschmidt/vitalog/commit/3678dc8f2651d61cf683f3f574a9b563dfea17ed))
* food_cmd nutrient scaling and output line formatting ([f91b00d](https://github.com/adrianschmidt/vitalog/commit/f91b00dc7e191d38f3fd0d15b0c71661c7994ce3))
* include nutrition_db in daylog status --json ([7c7f0a0](https://github.com/adrianschmidt/vitalog/commit/7c7f0a083565e27af2a19d3e5f459c8b026535e7))
* include nutrition-db.md in sync_all and rebuild_all ([825888e](https://github.com/adrianschmidt/vitalog/commit/825888e60366d9216fc0111998f577b1341c496e))
* **legacy:** add detection helpers for daylog-era paths ([f4363a2](https://github.com/adrianschmidt/vitalog/commit/f4363a2921d67e198c27c1d40ca82733139e7625))
* **legacy:** add migrate_config_dir with idempotency + refuse-overwrite ([c524ab1](https://github.com/adrianschmidt/vitalog/commit/c524ab1e18962b072a506eb4ea580b0e5ae8d365))
* **legacy:** add migrate_db with sidecar handling + refuse-overwrite ([3f64616](https://github.com/adrianschmidt/vitalog/commit/3f6461628822ac7879cc242ecacc10eb68c590c9))
* log sleep validates and normalizes per time_format ([e26f41a](https://github.com/adrianschmidt/vitalog/commit/e26f41a04cc65c8b00eb3ff01e3421250c61db90))
* materialize_nutrition_db reads file and replaces foods table ([9a57f47](https://github.com/adrianschmidt/vitalog/commit/9a57f474ce4c090f560710f064e19348962b209a))
* normalize sleep times to canonical 24h in DB ([b6d89d9](https://github.com/adrianschmidt/vitalog/commit/b6d89d97fc3068b5f0924f98aa446ac2bc95d263))
* NotesConfig with [notes.aliases] mapping ([c02c809](https://github.com/adrianschmidt/vitalog/commit/c02c809194b363db5c83a18c42b4c4e52e31b107))
* parse `## Food` section into macro totals (food_sum) ([a8b21de](https://github.com/adrianschmidt/vitalog/commit/a8b21de43a64396ad7f80caa94c7d5a4cb6912b1))
* parse goals.md frontmatter into suffix-keyed thresholds ([fea6249](https://github.com/adrianschmidt/vitalog/commit/fea62492b706e867fae8e51f90c15c7ce16760b9))
* register food/note/bp subcommands with stub handlers ([b0e766a](https://github.com/adrianschmidt/vitalog/commit/b0e766a5c498e635cc414a7fd6d6953888c797fa))
* render daily summary as JSON (today_cmd::render_json) ([e5326e5](https://github.com/adrianschmidt/vitalog/commit/e5326e569fcb5c75b371ca373a1e7afaaa1324d2))
* render daily summary text block (today_cmd::render_text) ([c3470ee](https://github.com/adrianschmidt/vitalog/commit/c3470eec6cae45e1c055ba2963fdc1a19df1eab2))
* split nutrition-db.md by ## headings + yaml blocks ([c619d70](https://github.com/adrianschmidt/vitalog/commit/c619d7033c67c1beb42267c6a6e3c451590d4182))
* tighten materialized_file_kind with hidden/swap filter ([b7fba45](https://github.com/adrianschmidt/vitalog/commit/b7fba45aef4692e4b40bcc16b498f3f7e8516796))
* watcher dispatches daily notes and nutrition-db.md ([a93cfba](https://github.com/adrianschmidt/vitalog/commit/a93cfbaf70d1e77b857a91ed0a1a105a8b8277aa))
* wire sleep-start and sleep-end into CLI ([f7a3dd3](https://github.com/adrianschmidt/vitalog/commit/f7a3dd3fe80a184167a390478111dccac1fc4072))
