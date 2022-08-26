# Introduction
* An alternative implementation of https://github.com/corsc/pagerduty-gcal

## Instructions
* Get client_id and client_secret from google api console (https://console.developers.google.com/projectselector/apis/credentials) for your project
* Get the pd_api_key from pagerduty
* Set these as environment variables
```
export GOOGLE_CLIENT_ID=xxxx
export GOOGLE_CLIENT_SECRET=yyyy
export PD_API_KEY=zzzz
```
* If you need to, build the binary with cargo build --release. You will find the final binary in target/release/xxxx
* Run the binary
```
target/release/gcal-pagerduty --start-date 2020-08-22 --duration-days 14 --pd-schedule PY8SSDL
```
