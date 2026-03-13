package io.github.phiresky.wayland_android;

import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * Parser for freedesktop.org Desktop Entry files (.desktop).
 * Implements the basic format from the Desktop Entry Specification:
 * https://specifications.freedesktop.org/desktop-entry/latest/
 *
 * Handles groups, key-value pairs, localized keys, escape sequences,
 * semicolon-separated lists, booleans, and comments.
 */
public class DesktopFileParser {

    /** Parsed representation of a .desktop file. */
    public static class DesktopFile {
        private final LinkedHashMap<String, LinkedHashMap<String, String>> groups =
                new LinkedHashMap<>();

        /** Get all entries in a group, or null if the group doesn't exist. */
        public Map<String, String> getGroup(String name) {
            return groups.get(name);
        }

        /** Get a raw string value from a group. Returns null if not found. */
        public String getString(String group, String key) {
            Map<String, String> g = groups.get(group);
            if (g == null) return null;
            String raw = g.get(key);
            return raw != null ? unescape(raw) : null;
        }

        /**
         * Get a localized string value. Tries locale variants in priority order:
         * lang_COUNTRY@MODIFIER, lang_COUNTRY, lang@MODIFIER, lang, then unlocalized.
         */
        public String getLocalized(String group, String key, String locale) {
            Map<String, String> g = groups.get(group);
            if (g == null) return null;

            if (locale != null && !locale.isEmpty()) {
                for (String variant : localeVariants(locale)) {
                    String raw = g.get(key + "[" + variant + "]");
                    if (raw != null) return unescape(raw);
                }
            }
            String raw = g.get(key);
            return raw != null ? unescape(raw) : null;
        }

        /** Get a boolean value. Returns false if not found or not "true". */
        public boolean getBoolean(String group, String key) {
            Map<String, String> g = groups.get(group);
            if (g == null) return false;
            return "true".equals(g.get(key));
        }

        /** Get a semicolon-separated list value. */
        public List<String> getStringList(String group, String key) {
            Map<String, String> g = groups.get(group);
            if (g == null) return List.of();
            String raw = g.get(key);
            if (raw == null) return List.of();
            return splitList(raw);
        }

        /** Convenience: get from the [Desktop Entry] group. */
        public String getString(String key) { return getString("Desktop Entry", key); }
        public String getLocalized(String key, String locale) { return getLocalized("Desktop Entry", key, locale); }
        public boolean getBoolean(String key) { return getBoolean("Desktop Entry", key); }
        public List<String> getStringList(String key) { return getStringList("Desktop Entry", key); }

        /** All group names in file order. */
        public List<String> getGroupNames() { return new ArrayList<>(groups.keySet()); }
    }

    /** Parse a .desktop file from its full text content. */
    public static DesktopFile parse(String content) {
        DesktopFile df = new DesktopFile();
        LinkedHashMap<String, String> currentGroup = null;

        for (String line : content.split("\n")) {
            line = line.trim();

            // Skip blank lines and comments
            if (line.isEmpty() || line.charAt(0) == '#') continue;

            // Group header
            if (line.charAt(0) == '[' && line.charAt(line.length() - 1) == ']') {
                String groupName = line.substring(1, line.length() - 1);
                currentGroup = df.groups.computeIfAbsent(groupName, k -> new LinkedHashMap<>());
                continue;
            }

            // Key=Value pair
            if (currentGroup != null) {
                int eq = line.indexOf('=');
                if (eq > 0) {
                    String key = line.substring(0, eq).trim();
                    String value = line.substring(eq + 1).trim();
                    currentGroup.put(key, value);
                }
            }
        }

        return df;
    }

    /**
     * Unescape a desktop entry string value.
     * Supports: \s (space), \n (newline), \t (tab), \r (carriage return),
     * \\ (backslash), \; (semicolon).
     */
    static String unescape(String value) {
        if (value.indexOf('\\') < 0) return value; // fast path

        StringBuilder sb = new StringBuilder(value.length());
        for (int i = 0; i < value.length(); i++) {
            char c = value.charAt(i);
            if (c == '\\' && i + 1 < value.length()) {
                char next = value.charAt(i + 1);
                switch (next) {
                    case 's':  sb.append(' ');  i++; break;
                    case 'n':  sb.append('\n'); i++; break;
                    case 't':  sb.append('\t'); i++; break;
                    case 'r':  sb.append('\r'); i++; break;
                    case '\\': sb.append('\\'); i++; break;
                    case ';':  sb.append(';');  i++; break;
                    default:   sb.append(c); break;
                }
            } else {
                sb.append(c);
            }
        }
        return sb.toString();
    }

    /**
     * Split a semicolon-separated list value, respecting \; escapes.
     * Trailing empty entries are preserved only if there's content after the last semicolon.
     */
    static List<String> splitList(String value) {
        List<String> items = new ArrayList<>();
        StringBuilder current = new StringBuilder();

        for (int i = 0; i < value.length(); i++) {
            char c = value.charAt(i);
            if (c == '\\' && i + 1 < value.length() && value.charAt(i + 1) == ';') {
                current.append(';');
                i++; // skip escaped semicolon
            } else if (c == ';') {
                items.add(unescape(current.toString()));
                current.setLength(0);
            } else {
                current.append(c);
            }
        }

        // Add trailing content (if any non-empty text after last semicolon)
        if (current.length() > 0) {
            items.add(unescape(current.toString()));
        }

        return items;
    }

    /**
     * Generate locale matching variants in priority order.
     * For "sr_YU@Latn" returns: ["sr_YU@Latn", "sr_YU", "sr@Latn", "sr"]
     */
    static List<String> localeVariants(String locale) {
        List<String> variants = new ArrayList<>(4);

        // Strip encoding (e.g. ".UTF-8") if present
        int dotIdx = locale.indexOf('.');
        if (dotIdx >= 0) {
            String afterDot = locale.substring(dotIdx + 1);
            int atIdx = afterDot.indexOf('@');
            if (atIdx >= 0) {
                locale = locale.substring(0, dotIdx) + "@" + afterDot.substring(atIdx + 1);
            } else {
                locale = locale.substring(0, dotIdx);
            }
        }

        String lang = locale;
        String country = null;
        String modifier = null;

        int atIdx = lang.indexOf('@');
        if (atIdx >= 0) {
            modifier = lang.substring(atIdx + 1);
            lang = lang.substring(0, atIdx);
        }

        int underIdx = lang.indexOf('_');
        if (underIdx >= 0) {
            country = lang.substring(underIdx + 1);
            lang = lang.substring(0, underIdx);
        }

        // Priority: lang_COUNTRY@MODIFIER > lang_COUNTRY > lang@MODIFIER > lang
        if (country != null && modifier != null) {
            variants.add(lang + "_" + country + "@" + modifier);
        }
        if (country != null) {
            variants.add(lang + "_" + country);
        }
        if (modifier != null) {
            variants.add(lang + "@" + modifier);
        }
        variants.add(lang);

        return variants;
    }
}
