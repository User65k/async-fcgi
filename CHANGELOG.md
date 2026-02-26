# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.1](https://github.com/User65k/async-fcgi/compare/v0.5.0...v0.5.1) - 2026-02-26

### Fixed

- fix params bigger than 0x7f ([#6](https://github.com/User65k/async-fcgi/pull/6))
- fixed test
- fixed uppercase content type and len
- fixed con_pool values request
- fixed RecordReader endless loop
- fixed unittest
- fixed unix stream address parsing

### Other

- release please ([#10](https://github.com/User65k/async-fcgi/pull/10))
- update to hyper v1.2 ([#9](https://github.com/User65k/async-fcgi/pull/9))
- Abort ([#7](https://github.com/User65k/async-fcgi/pull/7))
- docsrs
- docsrs feature ([#3](https://github.com/User65k/async-fcgi/pull/3))
- async-stream-connection only if web_server
- moved stream to own crate
- enums
- vers+dep+edition bump
- use size hint instead of header
- run test workflow and version
- TCP remove fix port nrs
- fmt
- code cleanups
- proper abort on multiline header error
- move cursor instead of copy
- added flush_data_chunk
- remove cargo lock
- use codec
- version update
- record.append to work with BufMut
- reduced copys, added http header strategies
- introduce codec feature
- version bump
- tokio 1.0
- badges
- clenups + dont error if request is done
- sane alive check
- refactor
- start FCGI app
- version for huge response bug
- handle huge response records
- GET_VALUES at ConPool creation
- cleanups
- init
- Initial commit

### Removed

- removed second loop
- removed allocations
