package io.github.phiresky.wayland_android

/**
 * Parser for freedesktop.org Desktop Entry files (.desktop).
 * Implements the basic format from the Desktop Entry Specification:
 * https://specifications.freedesktop.org/desktop-entry/latest/
 *
 * Handles groups, key-value pairs, localized keys, escape sequences,
 * semicolon-separated lists, booleans, and comments.
 */
object DesktopFileParser {

    /** Parsed representation of a .desktop file. */
    class DesktopFile {
        internal val groups = LinkedHashMap<String, LinkedHashMap<String, String>>()

        /** Get all entries in a group, or null if the group doesn't exist. */
        fun getGroup(name: String): Map<String, String>? = groups[name]

        /** Get a raw string value from a group. Returns null if not found. */
        fun getString(group: String, key: String): String? {
            val raw = groups[group]?.get(key) ?: return null
            return unescape(raw)
        }

        /**
         * Get a localized string value. Tries locale variants in priority order:
         * lang_COUNTRY@MODIFIER, lang_COUNTRY, lang@MODIFIER, lang, then unlocalized.
         */
        fun getLocalized(group: String, key: String, locale: String?): String? {
            val g = groups[group] ?: return null
            if (!locale.isNullOrEmpty()) {
                for (variant in localeVariants(locale)) {
                    val raw = g["$key[$variant]"]
                    if (raw != null) return unescape(raw)
                }
            }
            val raw = g[key]
            return if (raw != null) unescape(raw) else null
        }

        /** Get a boolean value. Returns false if not found or not "true". */
        fun getBoolean(group: String, key: String): Boolean =
            groups[group]?.get(key) == "true"

        /** Get a semicolon-separated list value. */
        fun getStringList(group: String, key: String): List<String> {
            val raw = groups[group]?.get(key) ?: return emptyList()
            return splitList(raw)
        }

        /** Convenience: get from the [Desktop Entry] group. */
        fun getString(key: String): String? = getString("Desktop Entry", key)
        fun getLocalized(key: String, locale: String?): String? = getLocalized("Desktop Entry", key, locale)
        fun getBoolean(key: String): Boolean = getBoolean("Desktop Entry", key)
        fun getStringList(key: String): List<String> = getStringList("Desktop Entry", key)

        /** All group names in file order. */
        fun getGroupNames(): List<String> = groups.keys.toList()
    }

    /** Parse a .desktop file from its full text content. */
    fun parse(content: String): DesktopFile {
        val df = DesktopFile()
        var currentGroup: LinkedHashMap<String, String>? = null

        for (rawLine in content.split("\n")) {
            val line = rawLine.trim()

            // Skip blank lines and comments
            if (line.isEmpty() || line[0] == '#') continue

            // Group header
            if (line[0] == '[' && line.last() == ']') {
                val groupName = line.substring(1, line.length - 1)
                currentGroup = df.groups.getOrPut(groupName) { LinkedHashMap() }
                continue
            }

            // Key=Value pair
            if (currentGroup != null) {
                val eq = line.indexOf('=')
                if (eq > 0) {
                    val key = line.substring(0, eq).trim()
                    val value = line.substring(eq + 1).trim()
                    currentGroup[key] = value
                }
            }
        }

        return df
    }

    /**
     * Unescape a desktop entry string value.
     * Supports: \s (space), \n (newline), \t (tab), \r (carriage return),
     * \\ (backslash), \; (semicolon).
     */
    internal fun unescape(value: String): String {
        if ('\\' !in value) return value // fast path

        val sb = StringBuilder(value.length)
        var i = 0
        while (i < value.length) {
            val c = value[i]
            if (c == '\\' && i + 1 < value.length) {
                when (value[i + 1]) {
                    's' -> { sb.append(' '); i++ }
                    'n' -> { sb.append('\n'); i++ }
                    't' -> { sb.append('\t'); i++ }
                    'r' -> { sb.append('\r'); i++ }
                    '\\' -> { sb.append('\\'); i++ }
                    ';' -> { sb.append(';'); i++ }
                    else -> sb.append(c)
                }
            } else {
                sb.append(c)
            }
            i++
        }
        return sb.toString()
    }

    /**
     * Split a semicolon-separated list value, respecting \; escapes.
     * Trailing empty entries are preserved only if there's content after the last semicolon.
     */
    internal fun splitList(value: String): List<String> {
        val items = mutableListOf<String>()
        val current = StringBuilder()

        var i = 0
        while (i < value.length) {
            val c = value[i]
            if (c == '\\' && i + 1 < value.length && value[i + 1] == ';') {
                current.append(';')
                i++ // skip escaped semicolon
            } else if (c == ';') {
                items.add(unescape(current.toString()))
                current.setLength(0)
            } else {
                current.append(c)
            }
            i++
        }

        // Add trailing content (if any non-empty text after last semicolon)
        if (current.isNotEmpty()) {
            items.add(unescape(current.toString()))
        }

        return items
    }

    /**
     * Generate locale matching variants in priority order.
     * For "sr_YU@Latn" returns: ["sr_YU@Latn", "sr_YU", "sr@Latn", "sr"]
     */
    internal fun localeVariants(locale: String): List<String> {
        var loc = locale
        val variants = mutableListOf<String>()

        // Strip encoding (e.g. ".UTF-8") if present
        val dotIdx = loc.indexOf('.')
        if (dotIdx >= 0) {
            val afterDot = loc.substring(dotIdx + 1)
            val atIdx = afterDot.indexOf('@')
            loc = if (atIdx >= 0) {
                loc.substring(0, dotIdx) + "@" + afterDot.substring(atIdx + 1)
            } else {
                loc.substring(0, dotIdx)
            }
        }

        var lang = loc
        var country: String? = null
        var modifier: String? = null

        val atIdx = lang.indexOf('@')
        if (atIdx >= 0) {
            modifier = lang.substring(atIdx + 1)
            lang = lang.substring(0, atIdx)
        }

        val underIdx = lang.indexOf('_')
        if (underIdx >= 0) {
            country = lang.substring(underIdx + 1)
            lang = lang.substring(0, underIdx)
        }

        // Priority: lang_COUNTRY@MODIFIER > lang_COUNTRY > lang@MODIFIER > lang
        if (country != null && modifier != null) {
            variants.add("${lang}_${country}@${modifier}")
        }
        if (country != null) {
            variants.add("${lang}_${country}")
        }
        if (modifier != null) {
            variants.add("${lang}@${modifier}")
        }
        variants.add(lang)

        return variants
    }
}
