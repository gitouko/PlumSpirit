# Composable Note Service

Install and enable this provider with services.provide. Then install the sibling
service-composer package; its README documents the complete workflow and limits.
The two algorithms are explicitly authored in main.lua. Provider callbacks receive
plain data, native namespace/version/seed context and a restricted worker scope.


This package is also embedded in **Plugins > Lua Plugins > Examples**; no download
or source checkout is needed. Installation starts disabled with no capabilities
granted by default. Review the listed permissions and enable it using the normal
plugin controls. Reinstall restores the shipped files and preserves your decisions.
