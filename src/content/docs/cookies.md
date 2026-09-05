---
title: Cookie policy
description: Everything this site stores in your browser, why it is stored, how long it lasts, and how to change your mind.
tableOfContents: false
---

This site documents `embassy-supervisor`, an open source Rust crate. It has no
accounts, advertising, or third-party content. It stores only two things: your
analytics consent choice, and a cached copy of the front page release list from
GitHub.

## What gets stored

| Name | Kind | What it is for | How long |
| --- | --- | --- | --- |
| `cc_cookie` | Cookie | Remembers your analytics choice so you are not asked again. | 182 days |
| `_ga`, `_ga_1SHQP5YBQ3` | Cookie | Google Analytics visitor and session identifiers. | About 2 years (Google's default) |
| `cc_loc` | Session storage | Country code from the consent region check. | Cleared when the tab closes |
| `sup-releases` | Local storage | Cached release list from GitHub for the front page ticker. | Replaced on next refresh, after 30 minutes at the earliest |

`cc_cookie` and `cc_loc` are always set; without them the site could not
remember your choice. The `_ga` cookies are set **only** if you accept
analytics, and are deleted if you turn analytics off.

## Being asked, or not

Whether you see a consent banner depends on where you are, because the rules
differ:

- **In the EEA, the UK and Switzerland**, analytics needs your agreement
  *before* anything is stored. You get a banner, and nothing is measured
  unless you accept.
- **Everywhere else**, analytics runs until you turn it off. There is no
  banner, and the **Cookie preferences** link in the footer turns it off.

To work out which applies, the page asks Cloudflare
(`cloudflare.com/cdn-cgi/trace`) which country your connection appears to come
from. That request necessarily shows Cloudflare your IP address, it happens
before you have chosen, and it is the reason the site can avoid asking people
who are not required to be asked. It sets no cookies, and the answer is kept
only for as long as the tab is open. If the check fails for any reason, the
site assumes you should be asked and shows the banner.

## Changing your mind

Use the **Cookie preferences** link in the footer of any documentation page.
It reopens the same panel the banner shows, and takes effect immediately.
Turning analytics off clears the `_ga` cookies.

Clearing your browser's cookies for this site also works, and puts you back to
being asked from scratch.

## What Google receives

If you accept analytics, Google Analytics records the page you viewed, a
rough location derived from your IP address, and general details about your
browser and device. It does not receive your name or email, this site runs no
advertising tags, and nothing here is used for profiling or sold to anyone.
Google's own description of what it does with the data is in the
[Google Privacy Policy](https://policies.google.com/privacy) and in
[how Google uses data from sites that use its services](https://policies.google.com/technologies/partner-sites).

## What GitHub receives

The front page fetches the latest release list from GitHub's API. Like any
request to GitHub, this reveals your IP address. It sets no cookies, needs no
consent, and the result is cached for half an hour. If GitHub does not answer,
no list is shown.

## The banner itself

The consent banner is
[vanilla-cookieconsent](https://github.com/orestbida/cookieconsent), served
from this site rather than from a content delivery network, so using this
page does not involve a consent vendor at all.

## Who is responsible, and how to ask

This site and the analytics described above are run by Cedric Rivard, who
decides what is collected and why. Questions about any of it, including
requests to see or delete what has been collected about you, go to
[batman782861@gmail.com](mailto:batman782861@gmail.com).

Requests are easier to act on if analytics is on, since with it off there is
no identifier tying anything to you in the first place.
