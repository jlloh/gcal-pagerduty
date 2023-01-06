# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.6] - 2023-01-06
- Allow automatic override of schedules from script, with a user prompt

### Changed
- Fixed some cyclic cases temporarily by checking last two swaps. Should find a better way around this

## [0.1.4] - 2022-09-02
### Changed
- Fixed some cyclic cases temporarily by checking last two swaps. Should find a better way around this

## [0.1.3] - 2022-08-27
### Added
- Better formatting of output with tabled crate

## [0.1.2] - 2022-08-26
### Added
- Removed oob flow and start a webserver on port 8080 for oidc authentication and token exchange