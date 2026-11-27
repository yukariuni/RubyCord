// @description Rewrites your outgoing messages by find and replace rules
// @version 0.1.1
// @author amaanq

const KEY = "rules";

interface Rule {
   find: string;
   replace: string;
   regex: RegExp | null;
   error: string | null;
}

// `find` is a literal unless it is `/pattern/flags`, which compiles to a regex.
// `$1` and `$&` in `replace` work in both forms, since a literal find becomes an
// escaped regex. An invalid regex keeps the rule inert and names its error.
const REGEX_FORM = /^\/(.+)\/([a-z]*)$/;

const escape = (text: string) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

function compile(find: string, replace: string): Rule {
   const form = REGEX_FORM.exec(find);
   const source = form ? form[1] : escape(find);
   const flags = form ? form[2] : "g";
   try {
      return { find, replace, regex: new RegExp(source, flags), error: null };
   } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      return { find, replace, regex: null, error: message };
   }
}

// One rule per line, `find=>replace`. `=>` inside a regex find stays with the
// find, since only the last `=>` splits the line.
function parse(text: string): Rule[] {
   const rules: Rule[] = [];
   for (const line of text.split("\n")) {
      if (!line.trim()) continue;
      const at = line.lastIndexOf("=>");
      if (at === -1) continue;
      rules.push(compile(line.slice(0, at).trim(), line.slice(at + 2)));
   }
   return rules;
}

let rules = parse(phoenix.storage.get<string>(KEY) ?? "");

const apply = (text: string) => {
   let out = text;
   for (const rule of rules) {
      if (rule.regex) out = out.replace(rule.regex, rule.replace);
   }
   return out;
};

phoenix.settings.define({
   rules: {
      type: "text",
      title: "Rules",
      description: "One per line, find=>replace. A find of /pattern/flags is a regex.",
      multiline: true,
      default: phoenix.storage.get<string>(KEY) ?? "",
      onChange(value) {
         phoenix.storage.set(KEY, value);
         rules = parse(value);
         const broken = rules.filter((rule) => rule.error);
         if (broken.length > 0) {
            phoenix.ui.toast(`${broken.length} rule(s) have an invalid regex`);
         }
      },
   },
});

phoenix.messages.onSend((text) => {
   if (rules.length === 0) return;
   const out = apply(text);
   return out === text ? undefined : out;
});
