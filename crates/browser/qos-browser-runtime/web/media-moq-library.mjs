/*! moq-dev/moq 3aedaf3503146c664479f6fc53a50b36bb5e816d; local authorization and bounded-cache hooks; MIT OR Apache-2.0; see vendor/moq/js/LICENSE-* */
var __create = Object.create;
var __defProp = Object.defineProperty;
var __getOwnPropDesc = Object.getOwnPropertyDescriptor;
var __getOwnPropNames = Object.getOwnPropertyNames;
var __getProtoOf = Object.getPrototypeOf;
var __hasOwnProp = Object.prototype.hasOwnProperty;
var __commonJS = (cb, mod) => function __require() {
  try {
    return mod || (0, cb[__getOwnPropNames(cb)[0]])((mod = { exports: {} }).exports, mod), mod.exports;
  } catch (e) {
    throw mod = 0, e;
  }
};
var __export = (target, all) => {
  for (var name in all)
    __defProp(target, name, { get: all[name], enumerable: true });
};
var __copyProps = (to, from2, except, desc) => {
  if (from2 && typeof from2 === "object" || typeof from2 === "function") {
    for (let key of __getOwnPropNames(from2))
      if (!__hasOwnProp.call(to, key) && key !== except)
        __defProp(to, key, { get: () => from2[key], enumerable: !(desc = __getOwnPropDesc(from2, key)) || desc.enumerable });
  }
  return to;
};
var __toESM = (mod, isNodeMode, target) => (target = mod != null ? __create(__getProtoOf(mod)) : {}, __copyProps(
  // If the importer is in node compatibility mode or this is not an ESM
  // file that has been converted to a CommonJS file using a Babel-
  // compatible transform (i.e. "__esModule" has not been set), then set
  // "default" to the CommonJS "module.exports" for node compatibility.
  isNodeMode || !mod || !mod.__esModule ? __defProp(target, "default", { value: mod, enumerable: true }) : target,
  mod
));

// scripts/moq-browser/node_modules/bowser/es5.js
var require_es5 = __commonJS({
  "scripts/moq-browser/node_modules/bowser/es5.js"(exports, module) {
    !(function(e, t) {
      "object" == typeof exports && "object" == typeof module ? module.exports = t() : "function" == typeof define && define.amd ? define([], t) : "object" == typeof exports ? exports.bowser = t() : e.bowser = t();
    })(exports, (function() {
      return (function(e) {
        var t = {};
        function r(i) {
          if (t[i]) return t[i].exports;
          var n = t[i] = { i, l: false, exports: {} };
          return e[i].call(n.exports, n, n.exports, r), n.l = true, n.exports;
        }
        return r.m = e, r.c = t, r.d = function(e2, t2, i) {
          r.o(e2, t2) || Object.defineProperty(e2, t2, { enumerable: true, get: i });
        }, r.r = function(e2) {
          "undefined" != typeof Symbol && Symbol.toStringTag && Object.defineProperty(e2, Symbol.toStringTag, { value: "Module" }), Object.defineProperty(e2, "__esModule", { value: true });
        }, r.t = function(e2, t2) {
          if (1 & t2 && (e2 = r(e2)), 8 & t2) return e2;
          if (4 & t2 && "object" == typeof e2 && e2 && e2.__esModule) return e2;
          var i = /* @__PURE__ */ Object.create(null);
          if (r.r(i), Object.defineProperty(i, "default", { enumerable: true, value: e2 }), 2 & t2 && "string" != typeof e2) for (var n in e2) r.d(i, n, function(t3) {
            return e2[t3];
          }.bind(null, n));
          return i;
        }, r.n = function(e2) {
          var t2 = e2 && e2.__esModule ? function() {
            return e2.default;
          } : function() {
            return e2;
          };
          return r.d(t2, "a", t2), t2;
        }, r.o = function(e2, t2) {
          return Object.prototype.hasOwnProperty.call(e2, t2);
        }, r.p = "", r(r.s = 90);
      })({ 17: function(e, t, r) {
        "use strict";
        t.__esModule = true, t.default = void 0;
        var i = r(18), n = (function() {
          function e2() {
          }
          return e2.getFirstMatch = function(e3, t2) {
            var r2 = t2.match(e3);
            return r2 && r2.length > 0 && r2[1] || "";
          }, e2.getSecondMatch = function(e3, t2) {
            var r2 = t2.match(e3);
            return r2 && r2.length > 1 && r2[2] || "";
          }, e2.matchAndReturnConst = function(e3, t2, r2) {
            if (e3.test(t2)) return r2;
          }, e2.getWindowsVersionName = function(e3) {
            switch (e3) {
              case "NT":
                return "NT";
              case "XP":
                return "XP";
              case "NT 5.0":
                return "2000";
              case "NT 5.1":
                return "XP";
              case "NT 5.2":
                return "2003";
              case "NT 6.0":
                return "Vista";
              case "NT 6.1":
                return "7";
              case "NT 6.2":
                return "8";
              case "NT 6.3":
                return "8.1";
              case "NT 10.0":
                return "10";
              default:
                return;
            }
          }, e2.getMacOSVersionName = function(e3) {
            var t2 = e3.split(".").splice(0, 2).map((function(e4) {
              return parseInt(e4, 10) || 0;
            }));
            t2.push(0);
            var r2 = t2[0], i2 = t2[1];
            if (10 === r2) switch (i2) {
              case 5:
                return "Leopard";
              case 6:
                return "Snow Leopard";
              case 7:
                return "Lion";
              case 8:
                return "Mountain Lion";
              case 9:
                return "Mavericks";
              case 10:
                return "Yosemite";
              case 11:
                return "El Capitan";
              case 12:
                return "Sierra";
              case 13:
                return "High Sierra";
              case 14:
                return "Mojave";
              case 15:
                return "Catalina";
              default:
                return;
            }
            switch (r2) {
              case 11:
                return "Big Sur";
              case 12:
                return "Monterey";
              case 13:
                return "Ventura";
              case 14:
                return "Sonoma";
              case 15:
                return "Sequoia";
              default:
                return;
            }
          }, e2.getAndroidVersionName = function(e3) {
            var t2 = e3.split(".").splice(0, 2).map((function(e4) {
              return parseInt(e4, 10) || 0;
            }));
            if (t2.push(0), !(1 === t2[0] && t2[1] < 5)) return 1 === t2[0] && t2[1] < 6 ? "Cupcake" : 1 === t2[0] && t2[1] >= 6 ? "Donut" : 2 === t2[0] && t2[1] < 2 ? "Eclair" : 2 === t2[0] && 2 === t2[1] ? "Froyo" : 2 === t2[0] && t2[1] > 2 ? "Gingerbread" : 3 === t2[0] ? "Honeycomb" : 4 === t2[0] && t2[1] < 1 ? "Ice Cream Sandwich" : 4 === t2[0] && t2[1] < 4 ? "Jelly Bean" : 4 === t2[0] && t2[1] >= 4 ? "KitKat" : 5 === t2[0] ? "Lollipop" : 6 === t2[0] ? "Marshmallow" : 7 === t2[0] ? "Nougat" : 8 === t2[0] ? "Oreo" : 9 === t2[0] ? "Pie" : void 0;
          }, e2.getVersionPrecision = function(e3) {
            return e3.split(".").length;
          }, e2.compareVersions = function(t2, r2, i2) {
            void 0 === i2 && (i2 = false);
            var n2 = e2.getVersionPrecision(t2), a = e2.getVersionPrecision(r2), o = Math.max(n2, a), s = 0, u = e2.map([t2, r2], (function(t3) {
              var r3 = o - e2.getVersionPrecision(t3), i3 = t3 + new Array(r3 + 1).join(".0");
              return e2.map(i3.split("."), (function(e3) {
                return new Array(20 - e3.length).join("0") + e3;
              })).reverse();
            }));
            for (i2 && (s = o - Math.min(n2, a)), o -= 1; o >= s; ) {
              if (u[0][o] > u[1][o]) return 1;
              if (u[0][o] === u[1][o]) {
                if (o === s) return 0;
                o -= 1;
              } else if (u[0][o] < u[1][o]) return -1;
            }
          }, e2.map = function(e3, t2) {
            var r2, i2 = [];
            if (Array.prototype.map) return Array.prototype.map.call(e3, t2);
            for (r2 = 0; r2 < e3.length; r2 += 1) i2.push(t2(e3[r2]));
            return i2;
          }, e2.find = function(e3, t2) {
            var r2, i2;
            if (Array.prototype.find) return Array.prototype.find.call(e3, t2);
            for (r2 = 0, i2 = e3.length; r2 < i2; r2 += 1) {
              var n2 = e3[r2];
              if (t2(n2, r2)) return n2;
            }
          }, e2.assign = function(e3) {
            for (var t2, r2, i2 = e3, n2 = arguments.length, a = new Array(n2 > 1 ? n2 - 1 : 0), o = 1; o < n2; o++) a[o - 1] = arguments[o];
            if (Object.assign) return Object.assign.apply(Object, [e3].concat(a));
            var s = function() {
              var e4 = a[t2];
              "object" == typeof e4 && null !== e4 && Object.keys(e4).forEach((function(t3) {
                i2[t3] = e4[t3];
              }));
            };
            for (t2 = 0, r2 = a.length; t2 < r2; t2 += 1) s();
            return e3;
          }, e2.getBrowserAlias = function(e3) {
            return i.BROWSER_ALIASES_MAP[e3];
          }, e2.getBrowserTypeByAlias = function(e3) {
            return i.BROWSER_MAP[e3] || "";
          }, e2;
        })();
        t.default = n, e.exports = t.default;
      }, 18: function(e, t, r) {
        "use strict";
        t.__esModule = true, t.ENGINE_MAP = t.OS_MAP = t.PLATFORMS_MAP = t.BROWSER_MAP = t.BROWSER_ALIASES_MAP = void 0;
        t.BROWSER_ALIASES_MAP = { AmazonBot: "amazonbot", "Amazon Silk": "amazon_silk", "Android Browser": "android", BaiduSpider: "baiduspider", Bada: "bada", BingCrawler: "bingcrawler", Brave: "brave", BlackBerry: "blackberry", "ChatGPT-User": "chatgpt_user", Chrome: "chrome", ClaudeBot: "claudebot", Chromium: "chromium", Diffbot: "diffbot", DuckDuckBot: "duckduckbot", DuckDuckGo: "duckduckgo", Electron: "electron", Epiphany: "epiphany", FacebookExternalHit: "facebookexternalhit", Firefox: "firefox", Focus: "focus", Generic: "generic", "Google Search": "google_search", Googlebot: "googlebot", GPTBot: "gptbot", "Internet Explorer": "ie", InternetArchiveCrawler: "internetarchivecrawler", "K-Meleon": "k_meleon", LibreWolf: "librewolf", Linespider: "linespider", Maxthon: "maxthon", "Meta-ExternalAds": "meta_externalads", "Meta-ExternalAgent": "meta_externalagent", "Meta-ExternalFetcher": "meta_externalfetcher", "Meta-WebIndexer": "meta_webindexer", "Microsoft Edge": "edge", "MZ Browser": "mz", "NAVER Whale Browser": "naver", "OAI-SearchBot": "oai_searchbot", Omgilibot: "omgilibot", Opera: "opera", "Opera Coast": "opera_coast", "Pale Moon": "pale_moon", PerplexityBot: "perplexitybot", "Perplexity-User": "perplexity_user", PhantomJS: "phantomjs", PingdomBot: "pingdombot", Puffin: "puffin", QQ: "qq", QQLite: "qqlite", QupZilla: "qupzilla", Roku: "roku", Safari: "safari", Sailfish: "sailfish", "Samsung Internet for Android": "samsung_internet", SlackBot: "slackbot", SeaMonkey: "seamonkey", Sleipnir: "sleipnir", "Sogou Browser": "sogou", Swing: "swing", Tizen: "tizen", "UC Browser": "uc", Vivaldi: "vivaldi", "WebOS Browser": "webos", WeChat: "wechat", YahooSlurp: "yahooslurp", "Yandex Browser": "yandex", YandexBot: "yandexbot", YouBot: "youbot" };
        t.BROWSER_MAP = { amazonbot: "AmazonBot", amazon_silk: "Amazon Silk", android: "Android Browser", baiduspider: "BaiduSpider", bada: "Bada", bingcrawler: "BingCrawler", blackberry: "BlackBerry", brave: "Brave", chatgpt_user: "ChatGPT-User", chrome: "Chrome", claudebot: "ClaudeBot", chromium: "Chromium", diffbot: "Diffbot", duckduckbot: "DuckDuckBot", duckduckgo: "DuckDuckGo", edge: "Microsoft Edge", electron: "Electron", epiphany: "Epiphany", facebookexternalhit: "FacebookExternalHit", firefox: "Firefox", focus: "Focus", generic: "Generic", google_search: "Google Search", googlebot: "Googlebot", gptbot: "GPTBot", ie: "Internet Explorer", internetarchivecrawler: "InternetArchiveCrawler", k_meleon: "K-Meleon", librewolf: "LibreWolf", linespider: "Linespider", maxthon: "Maxthon", meta_externalads: "Meta-ExternalAds", meta_externalagent: "Meta-ExternalAgent", meta_externalfetcher: "Meta-ExternalFetcher", meta_webindexer: "Meta-WebIndexer", mz: "MZ Browser", naver: "NAVER Whale Browser", oai_searchbot: "OAI-SearchBot", omgilibot: "Omgilibot", opera: "Opera", opera_coast: "Opera Coast", pale_moon: "Pale Moon", perplexitybot: "PerplexityBot", perplexity_user: "Perplexity-User", phantomjs: "PhantomJS", pingdombot: "PingdomBot", puffin: "Puffin", qq: "QQ Browser", qqlite: "QQ Browser Lite", qupzilla: "QupZilla", roku: "Roku", safari: "Safari", sailfish: "Sailfish", samsung_internet: "Samsung Internet for Android", seamonkey: "SeaMonkey", slackbot: "SlackBot", sleipnir: "Sleipnir", sogou: "Sogou Browser", swing: "Swing", tizen: "Tizen", uc: "UC Browser", vivaldi: "Vivaldi", webos: "WebOS Browser", wechat: "WeChat", yahooslurp: "YahooSlurp", yandex: "Yandex Browser", yandexbot: "YandexBot", youbot: "YouBot" };
        t.PLATFORMS_MAP = { bot: "bot", desktop: "desktop", mobile: "mobile", tablet: "tablet", tv: "tv" };
        t.OS_MAP = { Android: "Android", Bada: "Bada", BlackBerry: "BlackBerry", ChromeOS: "Chrome OS", HarmonyOS: "HarmonyOS", iOS: "iOS", Linux: "Linux", MacOS: "macOS", PlayStation4: "PlayStation 4", Roku: "Roku", Tizen: "Tizen", WebOS: "WebOS", Windows: "Windows", WindowsPhone: "Windows Phone" };
        t.ENGINE_MAP = { Blink: "Blink", EdgeHTML: "EdgeHTML", Gecko: "Gecko", Presto: "Presto", Trident: "Trident", WebKit: "WebKit" };
      }, 90: function(e, t, r) {
        "use strict";
        t.__esModule = true, t.default = void 0;
        var i, n = (i = r(91)) && i.__esModule ? i : { default: i }, a = r(18);
        function o(e2, t2) {
          for (var r2 = 0; r2 < t2.length; r2++) {
            var i2 = t2[r2];
            i2.enumerable = i2.enumerable || false, i2.configurable = true, "value" in i2 && (i2.writable = true), Object.defineProperty(e2, i2.key, i2);
          }
        }
        var s = (function() {
          function e2() {
          }
          var t2, r2, i2;
          return e2.getParser = function(e3, t3, r3) {
            if (void 0 === t3 && (t3 = false), void 0 === r3 && (r3 = null), "string" != typeof e3) throw new Error("UserAgent should be a string");
            return new n.default(e3, t3, r3);
          }, e2.parse = function(e3, t3) {
            return void 0 === t3 && (t3 = null), new n.default(e3, t3).getResult();
          }, t2 = e2, i2 = [{ key: "BROWSER_MAP", get: function() {
            return a.BROWSER_MAP;
          } }, { key: "ENGINE_MAP", get: function() {
            return a.ENGINE_MAP;
          } }, { key: "OS_MAP", get: function() {
            return a.OS_MAP;
          } }, { key: "PLATFORMS_MAP", get: function() {
            return a.PLATFORMS_MAP;
          } }], (r2 = null) && o(t2.prototype, r2), i2 && o(t2, i2), e2;
        })();
        t.default = s, e.exports = t.default;
      }, 91: function(e, t, r) {
        "use strict";
        t.__esModule = true, t.default = void 0;
        var i = u(r(92)), n = u(r(93)), a = u(r(94)), o = u(r(95)), s = u(r(17));
        function u(e2) {
          return e2 && e2.__esModule ? e2 : { default: e2 };
        }
        var d = (function() {
          function e2(e3, t3, r2) {
            if (void 0 === t3 && (t3 = false), void 0 === r2 && (r2 = null), null == e3 || "" === e3) throw new Error("UserAgent parameter can't be empty");
            this._ua = e3;
            var i2 = false;
            "boolean" == typeof t3 ? (i2 = t3, this._hints = r2) : this._hints = null != t3 && "object" == typeof t3 ? t3 : null, this.parsedResult = {}, true !== i2 && this.parse();
          }
          var t2 = e2.prototype;
          return t2.getHints = function() {
            return this._hints;
          }, t2.hasBrand = function(e3) {
            if (!this._hints || !Array.isArray(this._hints.brands)) return false;
            var t3 = e3.toLowerCase();
            return this._hints.brands.some((function(e4) {
              return e4.brand && e4.brand.toLowerCase() === t3;
            }));
          }, t2.getBrandVersion = function(e3) {
            if (this._hints && Array.isArray(this._hints.brands)) {
              var t3 = e3.toLowerCase(), r2 = this._hints.brands.find((function(e4) {
                return e4.brand && e4.brand.toLowerCase() === t3;
              }));
              return r2 ? r2.version : void 0;
            }
          }, t2.getUA = function() {
            return this._ua;
          }, t2.test = function(e3) {
            return e3.test(this._ua);
          }, t2.parseBrowser = function() {
            var e3 = this;
            this.parsedResult.browser = {};
            var t3 = s.default.find(i.default, (function(t4) {
              if ("function" == typeof t4.test) return t4.test(e3);
              if (Array.isArray(t4.test)) return t4.test.some((function(t5) {
                return e3.test(t5);
              }));
              throw new Error("Browser's test function is not valid");
            }));
            return t3 && (this.parsedResult.browser = t3.describe(this.getUA(), this)), this.parsedResult.browser;
          }, t2.getBrowser = function() {
            return this.parsedResult.browser ? this.parsedResult.browser : this.parseBrowser();
          }, t2.getBrowserName = function(e3) {
            return e3 ? String(this.getBrowser().name).toLowerCase() || "" : this.getBrowser().name || "";
          }, t2.getBrowserVersion = function() {
            return this.getBrowser().version;
          }, t2.getOS = function() {
            return this.parsedResult.os ? this.parsedResult.os : this.parseOS();
          }, t2.parseOS = function() {
            var e3 = this;
            this.parsedResult.os = {};
            var t3 = s.default.find(n.default, (function(t4) {
              if ("function" == typeof t4.test) return t4.test(e3);
              if (Array.isArray(t4.test)) return t4.test.some((function(t5) {
                return e3.test(t5);
              }));
              throw new Error("Browser's test function is not valid");
            }));
            return t3 && (this.parsedResult.os = t3.describe(this.getUA())), this.parsedResult.os;
          }, t2.getOSName = function(e3) {
            var t3 = this.getOS().name;
            return e3 ? String(t3).toLowerCase() || "" : t3 || "";
          }, t2.getOSVersion = function() {
            return this.getOS().version;
          }, t2.getPlatform = function() {
            return this.parsedResult.platform ? this.parsedResult.platform : this.parsePlatform();
          }, t2.getPlatformType = function(e3) {
            void 0 === e3 && (e3 = false);
            var t3 = this.getPlatform().type;
            return e3 ? String(t3).toLowerCase() || "" : t3 || "";
          }, t2.parsePlatform = function() {
            var e3 = this;
            this.parsedResult.platform = {};
            var t3 = s.default.find(a.default, (function(t4) {
              if ("function" == typeof t4.test) return t4.test(e3);
              if (Array.isArray(t4.test)) return t4.test.some((function(t5) {
                return e3.test(t5);
              }));
              throw new Error("Browser's test function is not valid");
            }));
            return t3 && (this.parsedResult.platform = t3.describe(this.getUA())), this.parsedResult.platform;
          }, t2.getEngine = function() {
            return this.parsedResult.engine ? this.parsedResult.engine : this.parseEngine();
          }, t2.getEngineName = function(e3) {
            return e3 ? String(this.getEngine().name).toLowerCase() || "" : this.getEngine().name || "";
          }, t2.parseEngine = function() {
            var e3 = this;
            this.parsedResult.engine = {};
            var t3 = s.default.find(o.default, (function(t4) {
              if ("function" == typeof t4.test) return t4.test(e3);
              if (Array.isArray(t4.test)) return t4.test.some((function(t5) {
                return e3.test(t5);
              }));
              throw new Error("Browser's test function is not valid");
            }));
            return t3 && (this.parsedResult.engine = t3.describe(this.getUA())), this.parsedResult.engine;
          }, t2.parse = function() {
            return this.parseBrowser(), this.parseOS(), this.parsePlatform(), this.parseEngine(), this;
          }, t2.getResult = function() {
            return s.default.assign({}, this.parsedResult);
          }, t2.satisfies = function(e3) {
            var t3 = this, r2 = {}, i2 = 0, n2 = {}, a2 = 0;
            if (Object.keys(e3).forEach((function(t4) {
              var o3 = e3[t4];
              "string" == typeof o3 ? (n2[t4] = o3, a2 += 1) : "object" == typeof o3 && (r2[t4] = o3, i2 += 1);
            })), i2 > 0) {
              var o2 = Object.keys(r2), u2 = s.default.find(o2, (function(e4) {
                return t3.isOS(e4);
              }));
              if (u2) {
                var d2 = this.satisfies(r2[u2]);
                if (void 0 !== d2) return d2;
              }
              var c = s.default.find(o2, (function(e4) {
                return t3.isPlatform(e4);
              }));
              if (c) {
                var f = this.satisfies(r2[c]);
                if (void 0 !== f) return f;
              }
            }
            if (a2 > 0) {
              var l = Object.keys(n2), b = s.default.find(l, (function(e4) {
                return t3.isBrowser(e4, true);
              }));
              if (void 0 !== b) return this.compareVersion(n2[b]);
            }
          }, t2.isBrowser = function(e3, t3) {
            void 0 === t3 && (t3 = false);
            var r2 = this.getBrowserName().toLowerCase(), i2 = e3.toLowerCase(), n2 = s.default.getBrowserTypeByAlias(i2);
            return t3 && n2 && (i2 = n2.toLowerCase()), i2 === r2;
          }, t2.compareVersion = function(e3) {
            var t3 = [0], r2 = e3, i2 = false, n2 = this.getBrowserVersion();
            if ("string" == typeof n2) return ">" === e3[0] || "<" === e3[0] ? (r2 = e3.substr(1), "=" === e3[1] ? (i2 = true, r2 = e3.substr(2)) : t3 = [], ">" === e3[0] ? t3.push(1) : t3.push(-1)) : "=" === e3[0] ? r2 = e3.substr(1) : "~" === e3[0] && (i2 = true, r2 = e3.substr(1)), t3.indexOf(s.default.compareVersions(n2, r2, i2)) > -1;
          }, t2.isOS = function(e3) {
            return this.getOSName(true) === String(e3).toLowerCase();
          }, t2.isPlatform = function(e3) {
            return this.getPlatformType(true) === String(e3).toLowerCase();
          }, t2.isEngine = function(e3) {
            return this.getEngineName(true) === String(e3).toLowerCase();
          }, t2.is = function(e3, t3) {
            return void 0 === t3 && (t3 = false), this.isBrowser(e3, t3) || this.isOS(e3) || this.isPlatform(e3);
          }, t2.some = function(e3) {
            var t3 = this;
            return void 0 === e3 && (e3 = []), e3.some((function(e4) {
              return t3.is(e4);
            }));
          }, e2;
        })();
        t.default = d, e.exports = t.default;
      }, 92: function(e, t, r) {
        "use strict";
        t.__esModule = true, t.default = void 0;
        var i, n = (i = r(17)) && i.__esModule ? i : { default: i };
        var a = /version\/(\d+(\.?_?\d+)+)/i, o = [{ test: [/gptbot/i], describe: function(e2) {
          var t2 = { name: "GPTBot" }, r2 = n.default.getFirstMatch(/gptbot\/(\d+(\.\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/chatgpt-user/i], describe: function(e2) {
          var t2 = { name: "ChatGPT-User" }, r2 = n.default.getFirstMatch(/chatgpt-user\/(\d+(\.\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/oai-searchbot/i], describe: function(e2) {
          var t2 = { name: "OAI-SearchBot" }, r2 = n.default.getFirstMatch(/oai-searchbot\/(\d+(\.\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/claudebot/i, /claude-web/i, /claude-user/i, /claude-searchbot/i], describe: function(e2) {
          var t2 = { name: "ClaudeBot" }, r2 = n.default.getFirstMatch(/(?:claudebot|claude-web|claude-user|claude-searchbot)\/(\d+(\.\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/omgilibot/i, /webzio-extended/i], describe: function(e2) {
          var t2 = { name: "Omgilibot" }, r2 = n.default.getFirstMatch(/(?:omgilibot|webzio-extended)\/(\d+(\.\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/diffbot/i], describe: function(e2) {
          var t2 = { name: "Diffbot" }, r2 = n.default.getFirstMatch(/diffbot\/(\d+(\.\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/perplexitybot/i], describe: function(e2) {
          var t2 = { name: "PerplexityBot" }, r2 = n.default.getFirstMatch(/perplexitybot\/(\d+(\.\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/perplexity-user/i], describe: function(e2) {
          var t2 = { name: "Perplexity-User" }, r2 = n.default.getFirstMatch(/perplexity-user\/(\d+(\.\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/youbot/i], describe: function(e2) {
          var t2 = { name: "YouBot" }, r2 = n.default.getFirstMatch(/youbot\/(\d+(\.\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/meta-webindexer/i], describe: function(e2) {
          var t2 = { name: "Meta-WebIndexer" }, r2 = n.default.getFirstMatch(/meta-webindexer\/(\d+(\.\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/meta-externalads/i], describe: function(e2) {
          var t2 = { name: "Meta-ExternalAds" }, r2 = n.default.getFirstMatch(/meta-externalads\/(\d+(\.\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/meta-externalagent/i], describe: function(e2) {
          var t2 = { name: "Meta-ExternalAgent" }, r2 = n.default.getFirstMatch(/meta-externalagent\/(\d+(\.\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/meta-externalfetcher/i], describe: function(e2) {
          var t2 = { name: "Meta-ExternalFetcher" }, r2 = n.default.getFirstMatch(/meta-externalfetcher\/(\d+(\.\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/googlebot/i], describe: function(e2) {
          var t2 = { name: "Googlebot" }, r2 = n.default.getFirstMatch(/googlebot\/(\d+(\.\d+))/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/linespider/i], describe: function(e2) {
          var t2 = { name: "Linespider" }, r2 = n.default.getFirstMatch(/(?:linespider)(?:-[-\w]+)?[\s/](\d+(\.\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/amazonbot/i], describe: function(e2) {
          var t2 = { name: "AmazonBot" }, r2 = n.default.getFirstMatch(/amazonbot\/(\d+(\.\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/bingbot/i], describe: function(e2) {
          var t2 = { name: "BingCrawler" }, r2 = n.default.getFirstMatch(/bingbot\/(\d+(\.\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/baiduspider/i], describe: function(e2) {
          var t2 = { name: "BaiduSpider" }, r2 = n.default.getFirstMatch(/baiduspider\/(\d+(\.\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/duckduckbot/i], describe: function(e2) {
          var t2 = { name: "DuckDuckBot" }, r2 = n.default.getFirstMatch(/duckduckbot\/(\d+(\.\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/ia_archiver/i], describe: function(e2) {
          var t2 = { name: "InternetArchiveCrawler" }, r2 = n.default.getFirstMatch(/ia_archiver\/(\d+(\.\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/facebookexternalhit/i, /facebookcatalog/i], describe: function() {
          return { name: "FacebookExternalHit" };
        } }, { test: [/slackbot/i, /slack-imgProxy/i], describe: function(e2) {
          var t2 = { name: "SlackBot" }, r2 = n.default.getFirstMatch(/(?:slackbot|slack-imgproxy)(?:-[-\w]+)?[\s/](\d+(\.\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/yahoo!?[\s/]*slurp/i], describe: function() {
          return { name: "YahooSlurp" };
        } }, { test: [/yandexbot/i, /yandexmobilebot/i], describe: function() {
          return { name: "YandexBot" };
        } }, { test: [/pingdom/i], describe: function() {
          return { name: "PingdomBot" };
        } }, { test: [/opera/i], describe: function(e2) {
          var t2 = { name: "Opera" }, r2 = n.default.getFirstMatch(a, e2) || n.default.getFirstMatch(/(?:opera)[\s/](\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/opr\/|opios/i], describe: function(e2) {
          var t2 = { name: "Opera" }, r2 = n.default.getFirstMatch(/(?:opr|opios)[\s/](\S+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/SamsungBrowser/i], describe: function(e2) {
          var t2 = { name: "Samsung Internet for Android" }, r2 = n.default.getFirstMatch(a, e2) || n.default.getFirstMatch(/(?:SamsungBrowser)[\s/](\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/Whale/i], describe: function(e2) {
          var t2 = { name: "NAVER Whale Browser" }, r2 = n.default.getFirstMatch(a, e2) || n.default.getFirstMatch(/(?:whale)[\s/](\d+(?:\.\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/PaleMoon/i], describe: function(e2) {
          var t2 = { name: "Pale Moon" }, r2 = n.default.getFirstMatch(a, e2) || n.default.getFirstMatch(/(?:PaleMoon)[\s/](\d+(?:\.\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/MZBrowser/i], describe: function(e2) {
          var t2 = { name: "MZ Browser" }, r2 = n.default.getFirstMatch(/(?:MZBrowser)[\s/](\d+(?:\.\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/focus/i], describe: function(e2) {
          var t2 = { name: "Focus" }, r2 = n.default.getFirstMatch(/(?:focus)[\s/](\d+(?:\.\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/swing/i], describe: function(e2) {
          var t2 = { name: "Swing" }, r2 = n.default.getFirstMatch(/(?:swing)[\s/](\d+(?:\.\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/coast/i], describe: function(e2) {
          var t2 = { name: "Opera Coast" }, r2 = n.default.getFirstMatch(a, e2) || n.default.getFirstMatch(/(?:coast)[\s/](\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/opt\/\d+(?:.?_?\d+)+/i], describe: function(e2) {
          var t2 = { name: "Opera Touch" }, r2 = n.default.getFirstMatch(/(?:opt)[\s/](\d+(\.?_?\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/yabrowser/i], describe: function(e2) {
          var t2 = { name: "Yandex Browser" }, r2 = n.default.getFirstMatch(/(?:yabrowser)[\s/](\d+(\.?_?\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/ucbrowser/i], describe: function(e2) {
          var t2 = { name: "UC Browser" }, r2 = n.default.getFirstMatch(a, e2) || n.default.getFirstMatch(/(?:ucbrowser)[\s/](\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/Maxthon|mxios/i], describe: function(e2) {
          var t2 = { name: "Maxthon" }, r2 = n.default.getFirstMatch(a, e2) || n.default.getFirstMatch(/(?:Maxthon|mxios)[\s/](\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/epiphany/i], describe: function(e2) {
          var t2 = { name: "Epiphany" }, r2 = n.default.getFirstMatch(a, e2) || n.default.getFirstMatch(/(?:epiphany)[\s/](\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/puffin/i], describe: function(e2) {
          var t2 = { name: "Puffin" }, r2 = n.default.getFirstMatch(a, e2) || n.default.getFirstMatch(/(?:puffin)[\s/](\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/sleipnir/i], describe: function(e2) {
          var t2 = { name: "Sleipnir" }, r2 = n.default.getFirstMatch(a, e2) || n.default.getFirstMatch(/(?:sleipnir)[\s/](\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/k-meleon/i], describe: function(e2) {
          var t2 = { name: "K-Meleon" }, r2 = n.default.getFirstMatch(a, e2) || n.default.getFirstMatch(/(?:k-meleon)[\s/](\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/micromessenger/i], describe: function(e2) {
          var t2 = { name: "WeChat" }, r2 = n.default.getFirstMatch(/(?:micromessenger)[\s/](\d+(\.?_?\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/qqbrowser/i], describe: function(e2) {
          var t2 = { name: /qqbrowserlite/i.test(e2) ? "QQ Browser Lite" : "QQ Browser" }, r2 = n.default.getFirstMatch(/(?:qqbrowserlite|qqbrowser)[/](\d+(\.?_?\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/msie|trident/i], describe: function(e2) {
          var t2 = { name: "Internet Explorer" }, r2 = n.default.getFirstMatch(/(?:msie |rv:)(\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/\sedg\//i], describe: function(e2) {
          var t2 = { name: "Microsoft Edge" }, r2 = n.default.getFirstMatch(/\sedg\/(\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/edg([ea]|ios)/i], describe: function(e2) {
          var t2 = { name: "Microsoft Edge" }, r2 = n.default.getSecondMatch(/edg([ea]|ios)\/(\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/vivaldi/i], describe: function(e2) {
          var t2 = { name: "Vivaldi" }, r2 = n.default.getFirstMatch(/vivaldi\/(\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/seamonkey/i], describe: function(e2) {
          var t2 = { name: "SeaMonkey" }, r2 = n.default.getFirstMatch(/seamonkey\/(\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/sailfish/i], describe: function(e2) {
          var t2 = { name: "Sailfish" }, r2 = n.default.getFirstMatch(/sailfish\s?browser\/(\d+(\.\d+)?)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/silk/i], describe: function(e2) {
          var t2 = { name: "Amazon Silk" }, r2 = n.default.getFirstMatch(/silk\/(\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/phantom/i], describe: function(e2) {
          var t2 = { name: "PhantomJS" }, r2 = n.default.getFirstMatch(/phantomjs\/(\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/slimerjs/i], describe: function(e2) {
          var t2 = { name: "SlimerJS" }, r2 = n.default.getFirstMatch(/slimerjs\/(\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/blackberry|\bbb\d+/i, /rim\stablet/i], describe: function(e2) {
          var t2 = { name: "BlackBerry" }, r2 = n.default.getFirstMatch(a, e2) || n.default.getFirstMatch(/blackberry[\d]+\/(\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/(web|hpw)[o0]s/i], describe: function(e2) {
          var t2 = { name: "WebOS Browser" }, r2 = n.default.getFirstMatch(a, e2) || n.default.getFirstMatch(/w(?:eb)?[o0]sbrowser\/(\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/bada/i], describe: function(e2) {
          var t2 = { name: "Bada" }, r2 = n.default.getFirstMatch(/dolfin\/(\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/tizen/i], describe: function(e2) {
          var t2 = { name: "Tizen" }, r2 = n.default.getFirstMatch(/(?:tizen\s?)?browser\/(\d+(\.?_?\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/qupzilla/i], describe: function(e2) {
          var t2 = { name: "QupZilla" }, r2 = n.default.getFirstMatch(/(?:qupzilla)[\s/](\d+(\.?_?\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/librewolf/i], describe: function(e2) {
          var t2 = { name: "LibreWolf" }, r2 = n.default.getFirstMatch(/(?:librewolf)[\s/](\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/firefox|iceweasel|fxios/i], describe: function(e2) {
          var t2 = { name: "Firefox" }, r2 = n.default.getFirstMatch(/(?:firefox|iceweasel|fxios)[\s/](\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/electron/i], describe: function(e2) {
          var t2 = { name: "Electron" }, r2 = n.default.getFirstMatch(/(?:electron)\/(\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/sogoumobilebrowser/i, /metasr/i, /se 2\.[x]/i], describe: function(e2) {
          var t2 = { name: "Sogou Browser" }, r2 = n.default.getFirstMatch(/(?:sogoumobilebrowser)[\s/](\d+(\.?_?\d+)+)/i, e2), i2 = n.default.getFirstMatch(/(?:chrome|crios|crmo)\/(\d+(\.?_?\d+)+)/i, e2), a2 = n.default.getFirstMatch(/se ([\d.]+)x/i, e2), o2 = r2 || i2 || a2;
          return o2 && (t2.version = o2), t2;
        } }, { test: [/MiuiBrowser/i], describe: function(e2) {
          var t2 = { name: "Miui" }, r2 = n.default.getFirstMatch(/(?:MiuiBrowser)[\s/](\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: function(e2) {
          return !!e2.hasBrand("DuckDuckGo") || e2.test(/\sDdg\/[\d.]+$/i);
        }, describe: function(e2, t2) {
          var r2 = { name: "DuckDuckGo" };
          if (t2) {
            var i2 = t2.getBrandVersion("DuckDuckGo");
            if (i2) return r2.version = i2, r2;
          }
          var a2 = n.default.getFirstMatch(/\sDdg\/([\d.]+)$/i, e2);
          return a2 && (r2.version = a2), r2;
        } }, { test: function(e2) {
          return e2.hasBrand("Brave");
        }, describe: function(e2, t2) {
          var r2 = { name: "Brave" };
          if (t2) {
            var i2 = t2.getBrandVersion("Brave");
            if (i2) return r2.version = i2, r2;
          }
          return r2;
        } }, { test: [/chromium/i], describe: function(e2) {
          var t2 = { name: "Chromium" }, r2 = n.default.getFirstMatch(/(?:chromium)[\s/](\d+(\.?_?\d+)+)/i, e2) || n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/chrome|crios|crmo/i], describe: function(e2) {
          var t2 = { name: "Chrome" }, r2 = n.default.getFirstMatch(/(?:chrome|crios|crmo)\/(\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/GSA/i], describe: function(e2) {
          var t2 = { name: "Google Search" }, r2 = n.default.getFirstMatch(/(?:GSA)\/(\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: function(e2) {
          var t2 = !e2.test(/like android/i), r2 = e2.test(/android/i);
          return t2 && r2;
        }, describe: function(e2) {
          var t2 = { name: "Android Browser" }, r2 = n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/playstation 4/i], describe: function(e2) {
          var t2 = { name: "PlayStation 4" }, r2 = n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/safari|applewebkit/i], describe: function(e2) {
          var t2 = { name: "Safari" }, r2 = n.default.getFirstMatch(a, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/.*/i], describe: function(e2) {
          var t2 = -1 !== e2.search("\\(") ? /^(.*)\/(.*)[ \t]\((.*)/ : /^(.*)\/(.*) /;
          return { name: n.default.getFirstMatch(t2, e2), version: n.default.getSecondMatch(t2, e2) };
        } }];
        t.default = o, e.exports = t.default;
      }, 93: function(e, t, r) {
        "use strict";
        t.__esModule = true, t.default = void 0;
        var i, n = (i = r(17)) && i.__esModule ? i : { default: i }, a = r(18);
        var o = [{ test: [/Roku\/DVP/], describe: function(e2) {
          var t2 = n.default.getFirstMatch(/Roku\/DVP-(\d+\.\d+)/i, e2);
          return { name: a.OS_MAP.Roku, version: t2 };
        } }, { test: [/windows phone/i], describe: function(e2) {
          var t2 = n.default.getFirstMatch(/windows phone (?:os)?\s?(\d+(\.\d+)*)/i, e2);
          return { name: a.OS_MAP.WindowsPhone, version: t2 };
        } }, { test: [/windows /i], describe: function(e2) {
          var t2 = n.default.getFirstMatch(/Windows ((NT|XP)( \d\d?.\d)?)/i, e2), r2 = n.default.getWindowsVersionName(t2);
          return { name: a.OS_MAP.Windows, version: t2, versionName: r2 };
        } }, { test: [/Macintosh(.*?) FxiOS(.*?)\//], describe: function(e2) {
          var t2 = { name: a.OS_MAP.iOS }, r2 = n.default.getSecondMatch(/(Version\/)(\d[\d.]+)/, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/macintosh/i], describe: function(e2) {
          var t2 = n.default.getFirstMatch(/mac os x (\d+(\.?_?\d+)+)/i, e2).replace(/[_\s]/g, "."), r2 = n.default.getMacOSVersionName(t2), i2 = { name: a.OS_MAP.MacOS, version: t2 };
          return r2 && (i2.versionName = r2), i2;
        } }, { test: [/(ipod|iphone|ipad)/i], describe: function(e2) {
          var t2 = n.default.getFirstMatch(/os (\d+([_\s]\d+)*) like mac os x/i, e2).replace(/[_\s]/g, ".");
          return { name: a.OS_MAP.iOS, version: t2 };
        } }, { test: [/OpenHarmony/i], describe: function(e2) {
          var t2 = n.default.getFirstMatch(/OpenHarmony\s+(\d+(\.\d+)*)/i, e2);
          return { name: a.OS_MAP.HarmonyOS, version: t2 };
        } }, { test: function(e2) {
          var t2 = !e2.test(/like android/i), r2 = e2.test(/android/i);
          return t2 && r2;
        }, describe: function(e2) {
          var t2 = n.default.getFirstMatch(/android[\s/-](\d+(\.\d+)*)/i, e2), r2 = n.default.getAndroidVersionName(t2), i2 = { name: a.OS_MAP.Android, version: t2 };
          return r2 && (i2.versionName = r2), i2;
        } }, { test: [/(web|hpw)[o0]s/i], describe: function(e2) {
          var t2 = n.default.getFirstMatch(/(?:web|hpw)[o0]s\/(\d+(\.\d+)*)/i, e2), r2 = { name: a.OS_MAP.WebOS };
          return t2 && t2.length && (r2.version = t2), r2;
        } }, { test: [/blackberry|\bbb\d+/i, /rim\stablet/i], describe: function(e2) {
          var t2 = n.default.getFirstMatch(/rim\stablet\sos\s(\d+(\.\d+)*)/i, e2) || n.default.getFirstMatch(/blackberry\d+\/(\d+([_\s]\d+)*)/i, e2) || n.default.getFirstMatch(/\bbb(\d+)/i, e2);
          return { name: a.OS_MAP.BlackBerry, version: t2 };
        } }, { test: [/bada/i], describe: function(e2) {
          var t2 = n.default.getFirstMatch(/bada\/(\d+(\.\d+)*)/i, e2);
          return { name: a.OS_MAP.Bada, version: t2 };
        } }, { test: [/tizen/i], describe: function(e2) {
          var t2 = n.default.getFirstMatch(/tizen[/\s](\d+(\.\d+)*)/i, e2);
          return { name: a.OS_MAP.Tizen, version: t2 };
        } }, { test: [/linux/i], describe: function() {
          return { name: a.OS_MAP.Linux };
        } }, { test: [/CrOS/], describe: function() {
          return { name: a.OS_MAP.ChromeOS };
        } }, { test: [/PlayStation 4/], describe: function(e2) {
          var t2 = n.default.getFirstMatch(/PlayStation 4[/\s](\d+(\.\d+)*)/i, e2);
          return { name: a.OS_MAP.PlayStation4, version: t2 };
        } }];
        t.default = o, e.exports = t.default;
      }, 94: function(e, t, r) {
        "use strict";
        t.__esModule = true, t.default = void 0;
        var i, n = (i = r(17)) && i.__esModule ? i : { default: i }, a = r(18);
        var o = [{ test: [/googlebot/i], describe: function() {
          return { type: a.PLATFORMS_MAP.bot, vendor: "Google" };
        } }, { test: [/linespider/i], describe: function() {
          return { type: a.PLATFORMS_MAP.bot, vendor: "Line" };
        } }, { test: [/amazonbot/i], describe: function() {
          return { type: a.PLATFORMS_MAP.bot, vendor: "Amazon" };
        } }, { test: [/gptbot/i], describe: function() {
          return { type: a.PLATFORMS_MAP.bot, vendor: "OpenAI" };
        } }, { test: [/chatgpt-user/i], describe: function() {
          return { type: a.PLATFORMS_MAP.bot, vendor: "OpenAI" };
        } }, { test: [/oai-searchbot/i], describe: function() {
          return { type: a.PLATFORMS_MAP.bot, vendor: "OpenAI" };
        } }, { test: [/baiduspider/i], describe: function() {
          return { type: a.PLATFORMS_MAP.bot, vendor: "Baidu" };
        } }, { test: [/bingbot/i], describe: function() {
          return { type: a.PLATFORMS_MAP.bot, vendor: "Bing" };
        } }, { test: [/duckduckbot/i], describe: function() {
          return { type: a.PLATFORMS_MAP.bot, vendor: "DuckDuckGo" };
        } }, { test: [/claudebot/i, /claude-web/i, /claude-user/i, /claude-searchbot/i], describe: function() {
          return { type: a.PLATFORMS_MAP.bot, vendor: "Anthropic" };
        } }, { test: [/omgilibot/i, /webzio-extended/i], describe: function() {
          return { type: a.PLATFORMS_MAP.bot, vendor: "Webz.io" };
        } }, { test: [/diffbot/i], describe: function() {
          return { type: a.PLATFORMS_MAP.bot, vendor: "Diffbot" };
        } }, { test: [/perplexitybot/i], describe: function() {
          return { type: a.PLATFORMS_MAP.bot, vendor: "Perplexity AI" };
        } }, { test: [/perplexity-user/i], describe: function() {
          return { type: a.PLATFORMS_MAP.bot, vendor: "Perplexity AI" };
        } }, { test: [/youbot/i], describe: function() {
          return { type: a.PLATFORMS_MAP.bot, vendor: "You.com" };
        } }, { test: [/ia_archiver/i], describe: function() {
          return { type: a.PLATFORMS_MAP.bot, vendor: "Internet Archive" };
        } }, { test: [/meta-webindexer/i], describe: function() {
          return { type: a.PLATFORMS_MAP.bot, vendor: "Meta" };
        } }, { test: [/meta-externalads/i], describe: function() {
          return { type: a.PLATFORMS_MAP.bot, vendor: "Meta" };
        } }, { test: [/meta-externalagent/i], describe: function() {
          return { type: a.PLATFORMS_MAP.bot, vendor: "Meta" };
        } }, { test: [/meta-externalfetcher/i], describe: function() {
          return { type: a.PLATFORMS_MAP.bot, vendor: "Meta" };
        } }, { test: [/facebookexternalhit/i, /facebookcatalog/i], describe: function() {
          return { type: a.PLATFORMS_MAP.bot, vendor: "Meta" };
        } }, { test: [/slackbot/i, /slack-imgProxy/i], describe: function() {
          return { type: a.PLATFORMS_MAP.bot, vendor: "Slack" };
        } }, { test: [/yahoo/i], describe: function() {
          return { type: a.PLATFORMS_MAP.bot, vendor: "Yahoo" };
        } }, { test: [/yandexbot/i, /yandexmobilebot/i], describe: function() {
          return { type: a.PLATFORMS_MAP.bot, vendor: "Yandex" };
        } }, { test: [/pingdom/i], describe: function() {
          return { type: a.PLATFORMS_MAP.bot, vendor: "Pingdom" };
        } }, { test: [/huawei/i], describe: function(e2) {
          var t2 = n.default.getFirstMatch(/(can-l01)/i, e2) && "Nova", r2 = { type: a.PLATFORMS_MAP.mobile, vendor: "Huawei" };
          return t2 && (r2.model = t2), r2;
        } }, { test: [/nexus\s*(?:7|8|9|10).*/i], describe: function() {
          return { type: a.PLATFORMS_MAP.tablet, vendor: "Nexus" };
        } }, { test: [/ipad/i], describe: function() {
          return { type: a.PLATFORMS_MAP.tablet, vendor: "Apple", model: "iPad" };
        } }, { test: [/Macintosh(.*?) FxiOS(.*?)\//], describe: function() {
          return { type: a.PLATFORMS_MAP.tablet, vendor: "Apple", model: "iPad" };
        } }, { test: [/kftt build/i], describe: function() {
          return { type: a.PLATFORMS_MAP.tablet, vendor: "Amazon", model: "Kindle Fire HD 7" };
        } }, { test: [/silk/i], describe: function() {
          return { type: a.PLATFORMS_MAP.tablet, vendor: "Amazon" };
        } }, { test: [/tablet(?! pc)/i], describe: function() {
          return { type: a.PLATFORMS_MAP.tablet };
        } }, { test: function(e2) {
          var t2 = e2.test(/ipod|iphone/i), r2 = e2.test(/like (ipod|iphone)/i);
          return t2 && !r2;
        }, describe: function(e2) {
          var t2 = n.default.getFirstMatch(/(ipod|iphone)/i, e2);
          return { type: a.PLATFORMS_MAP.mobile, vendor: "Apple", model: t2 };
        } }, { test: [/nexus\s*[0-6].*/i, /galaxy nexus/i], describe: function() {
          return { type: a.PLATFORMS_MAP.mobile, vendor: "Nexus" };
        } }, { test: [/Nokia/i], describe: function(e2) {
          var t2 = n.default.getFirstMatch(/Nokia\s+([0-9]+(\.[0-9]+)?)/i, e2), r2 = { type: a.PLATFORMS_MAP.mobile, vendor: "Nokia" };
          return t2 && (r2.model = t2), r2;
        } }, { test: [/[^-]mobi/i], describe: function() {
          return { type: a.PLATFORMS_MAP.mobile };
        } }, { test: function(e2) {
          return "blackberry" === e2.getBrowserName(true);
        }, describe: function() {
          return { type: a.PLATFORMS_MAP.mobile, vendor: "BlackBerry" };
        } }, { test: function(e2) {
          return "bada" === e2.getBrowserName(true);
        }, describe: function() {
          return { type: a.PLATFORMS_MAP.mobile };
        } }, { test: function(e2) {
          return "windows phone" === e2.getBrowserName();
        }, describe: function() {
          return { type: a.PLATFORMS_MAP.mobile, vendor: "Microsoft" };
        } }, { test: function(e2) {
          var t2 = Number(String(e2.getOSVersion()).split(".")[0]);
          return "android" === e2.getOSName(true) && t2 >= 3;
        }, describe: function() {
          return { type: a.PLATFORMS_MAP.tablet };
        } }, { test: function(e2) {
          return "android" === e2.getOSName(true);
        }, describe: function() {
          return { type: a.PLATFORMS_MAP.mobile };
        } }, { test: [/smart-?tv|smarttv/i], describe: function() {
          return { type: a.PLATFORMS_MAP.tv };
        } }, { test: [/netcast/i], describe: function() {
          return { type: a.PLATFORMS_MAP.tv };
        } }, { test: function(e2) {
          return "macos" === e2.getOSName(true);
        }, describe: function() {
          return { type: a.PLATFORMS_MAP.desktop, vendor: "Apple" };
        } }, { test: function(e2) {
          return "windows" === e2.getOSName(true);
        }, describe: function() {
          return { type: a.PLATFORMS_MAP.desktop };
        } }, { test: function(e2) {
          return "linux" === e2.getOSName(true);
        }, describe: function() {
          return { type: a.PLATFORMS_MAP.desktop };
        } }, { test: function(e2) {
          return "playstation 4" === e2.getOSName(true);
        }, describe: function() {
          return { type: a.PLATFORMS_MAP.tv };
        } }, { test: function(e2) {
          return "roku" === e2.getOSName(true);
        }, describe: function() {
          return { type: a.PLATFORMS_MAP.tv };
        } }];
        t.default = o, e.exports = t.default;
      }, 95: function(e, t, r) {
        "use strict";
        t.__esModule = true, t.default = void 0;
        var i, n = (i = r(17)) && i.__esModule ? i : { default: i }, a = r(18);
        var o = [{ test: function(e2) {
          return "microsoft edge" === e2.getBrowserName(true);
        }, describe: function(e2) {
          if (/\sedg\//i.test(e2)) return { name: a.ENGINE_MAP.Blink };
          var t2 = n.default.getFirstMatch(/edge\/(\d+(\.?_?\d+)+)/i, e2);
          return { name: a.ENGINE_MAP.EdgeHTML, version: t2 };
        } }, { test: [/trident/i], describe: function(e2) {
          var t2 = { name: a.ENGINE_MAP.Trident }, r2 = n.default.getFirstMatch(/trident\/(\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: function(e2) {
          return e2.test(/presto/i);
        }, describe: function(e2) {
          var t2 = { name: a.ENGINE_MAP.Presto }, r2 = n.default.getFirstMatch(/presto\/(\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: function(e2) {
          var t2 = e2.test(/gecko/i), r2 = e2.test(/like gecko/i);
          return t2 && !r2;
        }, describe: function(e2) {
          var t2 = { name: a.ENGINE_MAP.Gecko }, r2 = n.default.getFirstMatch(/gecko\/(\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }, { test: [/(apple)?webkit\/537\.36/i], describe: function() {
          return { name: a.ENGINE_MAP.Blink };
        } }, { test: [/(apple)?webkit/i], describe: function(e2) {
          var t2 = { name: a.ENGINE_MAP.WebKit }, r2 = n.default.getFirstMatch(/webkit\/(\d+(\.?_?\d+)+)/i, e2);
          return r2 && (t2.version = r2), t2;
        } }];
        t.default = o, e.exports = t.default;
      } });
    }));
  }
});

// vendor/moq/js/signals/src/index.ts
var src_exports = {};
__export(src_exports, {
  Computed: () => Computed,
  Effect: () => Effect,
  Once: () => Once,
  Signal: () => Signal,
  getter: () => getter,
  readonlys: () => readonlys
});
var noop = () => {
};
var DEV = typeof import.meta.env !== "undefined" && import.meta.env?.MODE !== "production";
var SIGNAL_BRAND = /* @__PURE__ */ Symbol.for("@moq/signals");
var GETTER_BRAND = /* @__PURE__ */ Symbol.for("@moq/signals.getter");
function branded(value, brand) {
  return typeof value === "object" && value !== null && brand in value;
}
var Signal = class _Signal {
  #value;
  #subscribers = /* @__PURE__ */ new Set();
  #changed = /* @__PURE__ */ new Set();
  // Microtask coalescing state
  #pending = false;
  #oldValue;
  #hasCapturedOldValue = false;
  #forceNotify = false;
  // Brands to identify this as a Signal (and a readable) across package instances.
  [SIGNAL_BRAND] = true;
  [GETTER_BRAND] = true;
  constructor(value) {
    this.#value = value;
  }
  /** Returns the value if it's already a Signal, otherwise wraps it in a new Signal. */
  static from(value) {
    if (branded(value, SIGNAL_BRAND)) {
      return value;
    }
    return new _Signal(value);
  }
  /** Returns the current value without subscribing. */
  peek() {
    return this.#value;
  }
  /**
   * Sets the current value, notifying subscribers if it changed.
   * Pass `notify` true to always notify or false to never notify.
   * A function is stored as the value; use {@link update} to transform instead.
   */
  set(value, notify) {
    if (!this.#hasCapturedOldValue) {
      this.#oldValue = this.#value;
      this.#hasCapturedOldValue = true;
    }
    this.#value = value;
    if (notify === false) return;
    if (notify === true) this.#forceNotify = true;
    if (this.#subscribers.size === 0 && this.#changed.size === 0) {
      this.#hasCapturedOldValue = false;
      this.#oldValue = void 0;
      this.#forceNotify = false;
      return;
    }
    if (this.#pending) return;
    this.#pending = true;
    queueMicrotask(() => this.#flush());
  }
  #flush() {
    this.#pending = false;
    this.#hasCapturedOldValue = false;
    const old = this.#oldValue;
    this.#oldValue = void 0;
    const force = this.#forceNotify;
    this.#forceNotify = false;
    if (!force && isEqual(old, this.#value)) return;
    const value = this.#value;
    const changed = this.#changed;
    this.#changed = /* @__PURE__ */ new Set();
    for (const fn of this.#subscribers) {
      try {
        fn(value);
      } catch (error2) {
        console.error("signal subscriber error", error2);
      }
    }
    for (const fn of changed) {
      try {
        fn(value);
      } catch (error2) {
        console.error("signal changed error", error2);
      }
    }
  }
  /** Sets the value to the result of `fn(prev)`, notifying subscribers unless `notify` is false. */
  update(fn, notify = true) {
    const value = fn(this.#value);
    this.set(value, notify);
  }
  /**
   * Mutates the current value in place via `fn`, returning `fn`'s result and
   * notifying subscribers unless `notify` is false.
   */
  mutate(fn, notify = true) {
    const r = fn(this.#value);
    this.set(this.#value, notify);
    return r;
  }
  /** Calls `fn` every time the value changes. Returns a function to unsubscribe. */
  subscribe(fn) {
    this.#subscribers.add(fn);
    if (DEV && this.#subscribers.size >= 100 && Number.isInteger(Math.log10(this.#subscribers.size))) {
      throw new Error("signal has too many subscribers; may be leaking");
    }
    return () => this.#subscribers.delete(fn);
  }
  changed(fn) {
    if (fn) {
      this.#changed.add(fn);
      return () => this.#changed.delete(fn);
    }
    return new Promise((resolve2) => {
      this.#changed.add(resolve2);
    });
  }
  /** Calls `fn` with the current value now, and again every time it changes. */
  watch(fn) {
    const dispose = this.subscribe(fn);
    queueMicrotask(() => fn(this.#value));
    return dispose;
  }
  /** Resolves with the next value from whichever of the given readables changes first. */
  static async race(...sigs) {
    const dispose = [];
    const result = await new Promise((resolve2) => {
      for (const sig of sigs) {
        dispose.push(sig.changed(resolve2));
      }
    });
    for (const fn of dispose) fn();
    return result;
  }
};
var Once = class {
  #signal = new Signal(void 0);
  // Brand to identify this as a readable across package instances.
  [GETTER_BRAND] = true;
  /** Settle the value. Throws if it has already settled. */
  set(value) {
    if (this.#signal.peek() !== void 0) {
      throw new Error("Once has already settled");
    }
    this.#signal.set(value);
  }
  /** The settled value, or `undefined` while still pending. */
  peek() {
    return this.#signal.peek();
  }
  changed(fn) {
    return fn ? this.#signal.changed(fn) : this.#signal.changed();
  }
  /** Calls `fn` when it settles (fires at most once). Returns a function to unsubscribe. */
  subscribe(fn) {
    return this.#signal.subscribe(fn);
  }
  /** Resolves with the settled value, immediately if it already settled. Never rejects on its own. */
  // biome-ignore lint/suspicious/noThenProperty: Once is intentionally awaitable (thenable).
  then(onFulfilled, onRejected) {
    const current = this.#signal.peek();
    const settled = current !== void 0 ? Promise.resolve(current) : this.#signal.changed().then((value) => value);
    return settled.then(onFulfilled, onRejected);
  }
};
function readonlys(signals) {
  return signals;
}
function getter(value) {
  if (branded(value, GETTER_BRAND) || branded(value, SIGNAL_BRAND)) {
    return value;
  }
  if (getterShaped(value)) {
    throw new Error("getter() requires a Signal, Computed, or Once; a foreign readable would become a constant");
  }
  return new Signal(value);
}
function getterShaped(value) {
  if (typeof value !== "object" || value === null) return false;
  const maybe = value;
  return typeof maybe.peek === "function" && typeof maybe.subscribe === "function" && typeof maybe.changed === "function";
}
var Effect = class _Effect {
  // Sanity check to make sure roots are being disposed on dev.
  static #finalizer = new FinalizationRegistry((debugInfo) => {
    console.warn(`Signals was garbage collected without being closed:
${debugInfo}`);
  });
  #fn;
  #dispose = [];
  // The teardown list a drain is walking, and how far it has got. Shared by the rerun and close
  // paths so a `close` landing inside a rerun's drain continues that pass instead of starting a
  // second one over callbacks that already ran. Set only while a drain is running.
  #draining;
  #drained = 0;
  #unwatch = [];
  #async = [];
  #stack;
  #scheduled = false;
  #stopped;
  #closed;
  #abort = new AbortController();
  #abortUsed = false;
  // True between a run's teardown and the start of the next one. A spawn task that resumes in
  // this window belongs to the run that just died, so its cleanup has to fire now.
  #stale = false;
  /** If a function is provided, it runs immediately and reruns whenever a tracked signal changes. */
  constructor(fn) {
    if (DEV) {
      const debug = new Error("created here:").stack ?? "No stack";
      _Effect.#finalizer.register(this, debug, this);
    }
    this.#fn = fn;
    if (DEV) {
      this.#stack = new Error().stack;
    }
    this.#stopped = Promise.withResolvers();
    this.#closed = Promise.withResolvers();
    if (fn) {
      this.#schedule();
    }
  }
  #schedule() {
    if (this.#scheduled) return;
    this.#scheduled = true;
    queueMicrotask(
      () => this.#run().catch((error2) => {
        console.error("effect error", error2, this.#stack);
      })
    );
  }
  async #run() {
    if (this.#dispose === void 0) return;
    this.#stale = true;
    this.#stopped.resolve();
    this.#abort.abort();
    for (const unwatch of this.#unwatch) unwatch();
    this.#unwatch.length = 0;
    const dispose = this.#dispose;
    this.#drain(dispose);
    if (this.#dispose === void 0) return;
    dispose.length = 0;
    if (this.#async.length > 0) {
      const warn = DEV ? setTimeout(() => {
        if (this.#dispose === void 0) return;
        console.warn(
          "spawn is still running after 5s; the effect cannot rerun until it settles",
          this.#stack
        );
      }, 5e3) : void 0;
      try {
        while (this.#dispose !== void 0 && this.#async.length > 0) {
          const pending = this.#async;
          this.#async = [];
          await Promise.race([Promise.all(pending), this.#closed.promise]);
        }
      } catch (error2) {
        console.error("async effect error", error2);
        if (this.#stack) console.error("stack", this.#stack);
      } finally {
        if (warn !== void 0) clearTimeout(warn);
      }
    }
    if (this.#dispose === void 0) return;
    this.#scheduled = false;
    this.#stale = false;
    this.#stopped = Promise.withResolvers();
    this.#abort = new AbortController();
    this.#abortUsed = false;
    if (this.#fn) {
      this.#fn(this);
      if (DEV && this.#dispose !== void 0 && this.#unwatch.length === 0 && this.#dispose.length === 0 && this.#async.length === 0 && !this.#abortUsed) {
        console.warn("Effect did not subscribe to any signals; it will never rerun.", this.#stack);
      }
    }
  }
  /** Reads a signal and tracks it, rerunning the effect whenever it changes. */
  get(signal) {
    if (this.#dispose === void 0) {
      if (DEV) {
        console.warn("Effect.get called when closed, returning current value");
      }
      return signal.peek();
    }
    const value = signal.peek();
    const dispose = signal.changed(() => this.#schedule());
    this.#unwatch.push(dispose);
    return value;
  }
  /**
   * Sets a signal for the duration of this run, restoring `cleanup` on rerun or close.
   * The cleanup value is optional only when the signal type includes `undefined`.
   */
  set(signal, value, ...args) {
    if (this.#dispose === void 0) {
      if (DEV) {
        console.warn("Effect.set called when closed, ignoring");
      }
      return;
    }
    signal.set(value);
    const cleanup = args[0];
    const cleanupValue = cleanup === void 0 ? void 0 : cleanup;
    this.cleanup(() => signal.set(cleanupValue));
  }
  /**
   * Runs an async task. The effect will not rerun until the task's promise settles.
   */
  // TODO: Add effect for another layer of nesting
  spawn(fn) {
    if (this.#dispose === void 0) {
      if (DEV) {
        console.warn("Effect.spawn called when closed");
      }
      return;
    }
    const promise = fn().catch((error2) => {
      console.error("spawn error", error2);
    });
    if (this.#dispose === void 0) return;
    this.#async.push(promise);
  }
  /** Runs `fn` after `ms` milliseconds, unless the effect reruns or closes first. */
  timer(fn, ms) {
    if (this.#dispose === void 0) {
      if (DEV) {
        console.warn("Effect.timer called when closed, ignoring");
      }
      return;
    }
    let timeout;
    timeout = setTimeout(() => {
      timeout = void 0;
      fn();
    }, ms);
    this.cleanup(() => timeout !== void 0 && clearTimeout(timeout));
  }
  /**
   * Runs `fn` as a nested effect, then closes that effect after `ms` milliseconds.
   *
   * Shares {@link run}'s handling of a task that outlived its run: the child is closed at the
   * next teardown, not immediately.
   */
  timeout(fn, ms) {
    if (this.#dispose === void 0) {
      if (DEV) {
        console.warn("Effect.timeout called when closed, ignoring");
      }
      return;
    }
    const effect = new _Effect(fn);
    let timeout = setTimeout(() => {
      effect.close();
      timeout = void 0;
    }, ms);
    this.#dispose.push(() => {
      if (timeout !== void 0) {
        clearTimeout(timeout);
        effect.close();
      }
    });
  }
  /** Runs `fn` on the next animation frame, unless the effect reruns or closes first. */
  animate(fn) {
    if (this.#dispose === void 0) {
      if (DEV) {
        console.warn("Effect.animate called when closed, ignoring");
      }
      return;
    }
    let animate = requestAnimationFrame((now) => {
      fn(now);
      animate = void 0;
    });
    this.cleanup(() => {
      if (animate !== void 0) cancelAnimationFrame(animate);
    });
  }
  /** Runs `fn` every `ms` milliseconds until the effect reruns or closes. */
  interval(fn, ms) {
    if (this.#dispose === void 0) {
      if (DEV) {
        console.warn("Effect.interval called when closed, ignoring");
      }
      return;
    }
    const interval = setInterval(() => {
      fn();
    }, ms);
    this.cleanup(() => clearInterval(interval));
  }
  /**
   * Creates a nested effect that reruns independently and is closed with its parent.
   *
   * Returns a disposer that closes the child early and releases it from the parent, so a long-lived
   * effect spawning a child per event (e.g. one per accepted subscription) doesn't accumulate dead
   * scopes until it finally reruns or closes.
   *
   * Called from a task that outlived its run, the child is closed at the *next* teardown rather
   * than immediately, unlike {@link cleanup}. Closing it now would cancel its first run before
   * `fn` executes, so whatever teardown `fn` registers would never fire at all.
   */
  run(fn) {
    if (this.#dispose === void 0) {
      if (DEV) {
        console.warn("Effect.run called when closed, ignoring");
      }
      return () => {
      };
    }
    const effect = new _Effect(fn);
    const dispose = () => effect.close();
    this.#dispose.push(dispose);
    return () => {
      effect.close();
      const disposers = this.#dispose;
      const index = disposers?.indexOf(dispose) ?? -1;
      if (disposers === void 0 || index === -1) return;
      if (this.#draining === disposers) disposers[index] = noop;
      else disposers.splice(index, 1);
    };
  }
  /** Creates a derived signal scoped to this effect, closed when the effect reruns or closes. */
  computed(fn) {
    const computed = new Computed(fn);
    this.cleanup(() => computed.close());
    return computed;
  }
  /** Reads and tracks several signals, returning their values or `undefined` if any is falsy. */
  getAll(signals) {
    const values = [];
    for (const signal of signals) {
      const value = this.get(signal);
      if (!value) return void 0;
      values.push(value);
    }
    return values;
  }
  /** Runs `fn` with the signal's value now and again whenever it changes, scoped to this effect. */
  subscribe(signal, fn) {
    if (this.#dispose === void 0) {
      if (DEV) {
        console.warn("Effect.subscribe called when closed, running once");
      }
      fn(signal.peek());
      return;
    }
    this.run((effect) => {
      const value = effect.get(signal);
      fn(value);
    });
  }
  event(target, type, listener, options) {
    if (this.#dispose === void 0) {
      if (DEV) {
        console.warn("Effect.eventListener called when closed, ignoring");
      }
      return;
    }
    const effectSignal = this.abort;
    const signal = typeof options !== "boolean" && options?.signal ? AbortSignal.any([effectSignal, options.signal]) : effectSignal;
    const merged = typeof options === "boolean" ? { capture: options, signal } : { ...options, signal };
    target.addEventListener(type, listener, merged);
  }
  /**
   * Registers a function to run when the effect reruns or closes.
   *
   * Runs `fn` immediately if the run that registered it is already over, which is what an
   * {@link spawn} task resuming after a rerun or close sees. Registering teardown is
   * therefore enough to own a resource, with no staleness check needed first.
   */
  /**
   * Run every callback in `list` exactly once, including ones registered while draining.
   *
   * Iterative, so a cascade of any depth stays flat rather than nesting a stack frame per link.
   * Re-entrant: a `close` from inside a rerun's teardown resumes this pass at the cursor instead
   * of replaying it. A callback that throws is reported and the rest still run, since teardown
   * that stops half way leaks whatever the remaining callbacks owned.
   */
  #drain(list) {
    const owner = this.#draining !== list;
    if (owner) {
      this.#draining = list;
      this.#drained = 0;
    }
    try {
      while (this.#drained < list.length) {
        const fn = list[this.#drained++];
        try {
          fn();
        } catch (error2) {
          console.error("cleanup error", error2, this.#stack);
        }
      }
    } finally {
      if (owner) this.#draining = void 0;
    }
  }
  cleanup(fn) {
    if (this.#dispose === void 0 || this.#stale) {
      if (this.#draining !== void 0) {
        this.#draining.push(fn);
        return;
      }
      fn();
      return;
    }
    this.#dispose.push(fn);
  }
  /** Stops the effect permanently, running all cleanup and unsubscribing from every signal. */
  close() {
    const dispose = this.#dispose;
    if (dispose === void 0) {
      return;
    }
    this.#dispose = void 0;
    this.#closed.resolve();
    this.#stopped.resolve();
    this.#abort.abort();
    this.#drain(dispose);
    for (const signal of this.#unwatch) signal();
    this.#unwatch.length = 0;
    this.#async.length = 0;
    if (DEV) {
      _Effect.#finalizer.unregister(this);
    }
  }
  /** Resolves when the effect is closed. */
  get closed() {
    return this.#closed.promise;
  }
  /** Resolves when the current run is about to be torn down, by a rerun or close. */
  get cancel() {
    return this.#stopped.promise;
  }
  /** An AbortSignal that fires when the current run is torn down. */
  get abort() {
    this.#abortUsed = true;
    return this.#abort.signal;
  }
  /** Copies `src` into `dst` and keeps `dst` in sync as `src` changes. */
  proxy(dst, src) {
    this.subscribe(src, (value) => dst.update(() => value));
  }
};
var Computed = class {
  #signal = new Signal(void 0);
  #effect;
  // Brand to identify this as a readable across package instances.
  [GETTER_BRAND] = true;
  /** Creates a computed that derives its value from `fn`, rerunning when dependencies change. */
  constructor(fn) {
    this.#effect = new Effect((effect) => {
      this.#signal.set(fn(effect));
    });
  }
  /** Returns the current derived value without subscribing (`undefined` until the first run). */
  peek() {
    return this.#signal.peek();
  }
  changed(fn) {
    return fn ? this.#signal.changed(fn) : this.#signal.changed();
  }
  /** Calls `fn` every time the derived value changes. */
  subscribe(fn) {
    return this.#signal.subscribe(fn);
  }
  /**
   * Stops recomputing and tracking dependencies. Required for standalone computeds;
   * an `effect.computed()` is closed automatically with its parent effect.
   */
  close() {
    this.#effect.close();
  }
};
function isEqual(a, b) {
  if (a === b) return true;
  if (a === null || b === null || typeof a !== "object" || typeof b !== "object") return false;
  const protoA = Object.getPrototypeOf(a);
  const protoB = Object.getPrototypeOf(b);
  if (protoA !== protoB) return false;
  if (protoA !== Object.prototype && protoA !== Array.prototype) return false;
  const keysA = Object.keys(a);
  const keysB = Object.keys(b);
  if (keysA.length !== keysB.length) return false;
  for (const key of keysA) {
    if (!isEqual(a[key], b[key])) return false;
  }
  return true;
}

// vendor/moq/js/net/src/announced.ts
var announced_exports = {};
__export(announced_exports, {
  Broadcast: () => Broadcast,
  Consumer: () => Consumer,
  Producer: () => Producer
});

// vendor/moq/js/net/src/path.ts
var path_exports = {};
__export(path_exports, {
  MAX_PARTS: () => MAX_PARTS,
  decode: () => decode,
  empty: () => empty,
  encode: () => encode,
  from: () => from,
  hasPrefix: () => hasPrefix,
  join: () => join,
  normalizeRelative: () => normalizeRelative,
  parts: () => parts,
  relative: () => relative,
  resolve: () => resolve,
  stripPrefix: () => stripPrefix,
  tryResolve: () => tryResolve
});
var MAX_PARTS = 32;
function from(...paths) {
  const joined = paths.join("/");
  return joined.replace(/\/+/g, "/").replace(/^\/+/, "").replace(/\/+$/, "");
}
function parts(path) {
  return path === "" ? [] : path.split("/");
}
function decode(raw) {
  const path = from(raw);
  return encode(path);
}
function encode(path) {
  if (parts(path).length > MAX_PARTS) {
    throw new Error(`path exceeds ${MAX_PARTS} parts`);
  }
  return path;
}
function hasPrefix(prefix, path) {
  if (prefix === "") {
    return true;
  }
  if (!path.startsWith(prefix)) {
    return false;
  }
  if (path.length === prefix.length) {
    return true;
  }
  return path[prefix.length] === "/";
}
function stripPrefix(prefix, path) {
  if (!hasPrefix(prefix, path)) {
    return null;
  }
  if (prefix === "") {
    return path;
  }
  if (path.length === prefix.length) {
    return "";
  }
  return path.slice(prefix.length + 1);
}
function join(path, other) {
  if (path === "") {
    return other;
  } else if (other === "") {
    return path;
  } else {
    return `${path}/${other}`;
  }
}
function empty() {
  return "";
}
function normalizeRelative(rel) {
  const raw = rel.split("/");
  const normalized = raw.filter((s) => s !== "" && s !== ".").join("/");
  return normalized === "" && raw.includes(".") ? "." : normalized;
}
function resolve(base, rel) {
  if (rel === "") return base;
  const segments = base === "" ? [] : base.split("/");
  segments.pop();
  for (const seg of rel.split("/")) {
    if (seg === "" || seg === ".") {
      continue;
    }
    if (seg === "..") {
      segments.pop();
    } else {
      segments.push(seg);
    }
  }
  return segments.join("/");
}
function tryResolve(base, rel) {
  if (rel === "") return base;
  const segments = base === "" ? [] : base.split("/");
  segments.pop();
  for (const seg of rel.split("/")) {
    if (seg === "" || seg === ".") {
      continue;
    }
    if (seg === "..") {
      if (segments.pop() === void 0) return void 0;
    } else {
      segments.push(seg);
    }
  }
  return segments.join("/");
}
function relative(target, base) {
  if (target === base) return "";
  const dir = base === "" ? [] : base.split("/");
  dir.pop();
  const parts2 = target === "" ? [] : target.split("/");
  let common = 0;
  while (common < dir.length && common < parts2.length && dir[common] === parts2[common]) {
    common += 1;
  }
  const down = parts2.slice(common);
  if (down.some((part) => part === "." || part === "..")) return void 0;
  const rel = Array(dir.length - common).fill("..").concat(down);
  return rel.length === 0 ? "." : rel.join("/");
}

// vendor/moq/js/net/src/announced.ts
var AnnounceState = class {
  queue = new Signal([]);
  closed = new Once();
};
function closeState(state, abort) {
  if (state.closed.peek() !== void 0) return;
  state.closed.set(abort ?? null);
  state.queue.mutate((queue) => {
    queue.length = 0;
  });
}
var Producer = class {
  /** Path prefix this stream is scoped to. */
  prefix;
  #state = new AnnounceState();
  constructor(prefix = empty()) {
    this.prefix = prefix;
  }
  /**
   * Settles once the stream closes: `null` on a clean close, or the abort {@link Error}.
   * Peek it synchronously (`undefined` while open), observe it reactively, or `await` it.
   */
  get closed() {
    return this.#state.closed;
  }
  /** A read handle for this announcement stream. */
  consume() {
    return makeConsumer(this.prefix, this.#state);
  }
  /** Writes an announcement to the queue. */
  append(event) {
    if (this.#state.closed.peek() !== void 0) throw new Error("announcements are closed");
    this.#state.queue.mutate((queue) => {
      queue.push(event);
    });
  }
  /** Closes the writer. Idempotent. */
  close(abort) {
    closeState(this.#state, abort);
  }
};
var makeConsumer;
var Consumer = class _Consumer {
  /** Path prefix this stream is scoped to. */
  prefix;
  #state;
  constructor(prefix, state) {
    this.prefix = prefix;
    this.#state = state;
  }
  /** Settles once the stream closes; see {@link Producer.closed}. */
  get closed() {
    return this.#state.closed;
  }
  static {
    makeConsumer = (prefix, state) => new _Consumer(prefix, state);
  }
  /** Returns the next announcement. */
  async next() {
    for (; ; ) {
      const announce = this.#state.queue.peek().shift();
      if (announce) return announce;
      const closed = this.#state.closed.peek();
      if (closed instanceof Error) throw closed;
      if (closed !== void 0) return void 0;
      await Signal.race(this.#state.queue, this.#state.closed);
    }
  }
  /** Closes the reader. Idempotent. */
  close(abort) {
    closeState(this.#state, abort);
  }
};
var warnedNoDiscovery = /* @__PURE__ */ new WeakSet();
var Broadcast = class {
  /** The broadcast path this handle watches. */
  path;
  /**
   * The live broadcast, or `undefined` while it is offline.
   *
   * Borrowed, not yours to close: this handle owns the consumer and swaps it when the path is
   * republished. `active` keeps pointing at whatever you closed, so once you drop the last
   * reference the shared broadcast is gone and reads fail until the next announcement replaces
   * it. Take a {@link broadcast.Consumer.clone} for a lifetime of your own, or close this whole
   * handle to release everything.
   */
  active;
  #active = new Signal(void 0);
  #signals = new Effect();
  /**
   * Watch a path on a connection.
   *
   * Prefer `announcedBroadcast(path)` on the connection itself. Reach for this when the
   * session you want to follow isn't either connection type, e.g. your own
   * `Getter<Established | undefined>`.
   */
  constructor({ connection, path }) {
    this.path = path;
    this.active = this.#active;
    const source = getter(connection);
    this.#signals.run((effect) => {
      const conn = effect.get(source);
      if (!conn) return;
      if (!conn.discovery) {
        if (!warnedNoDiscovery.has(conn)) {
          warnedNoDiscovery.add(conn);
          console.warn("relay does not support broadcast discovery; consuming without waiting.");
        }
        const blind = conn.consume(path);
        effect.cleanup(() => blind.close());
        effect.set(this.#active, blind, void 0);
        effect.spawn(async () => {
          await Promise.race([effect.cancel, conn.closed]);
          if (this.#active.peek() === blind) this.#active.set(void 0);
        });
        return;
      }
      const announced = conn.announced(path);
      effect.cleanup(() => announced.close());
      let current;
      const offline = () => {
        const mine = current;
        current?.close();
        current = void 0;
        if (this.#active.peek() === mine) this.#active.set(void 0);
      };
      effect.cleanup(offline);
      effect.spawn(async () => {
        try {
          for (; ; ) {
            const event = await Promise.race([effect.cancel, announced.next()]);
            if (!event) break;
            if (event.path !== empty()) continue;
            if (event.active) {
              if (current && current.closed.peek() === void 0) continue;
              current?.close();
              current = conn.consume(path);
              this.#active.set(current);
            } else {
              offline();
            }
          }
        } catch (err) {
          console.warn("broadcast discovery failed", err);
        }
        offline();
      });
    });
  }
  /** Closes the handle and the broadcast it currently holds. Idempotent. */
  close() {
    this.#signals.close();
  }
};

// vendor/moq/js/net/src/broadcast.ts
var broadcast_exports = {};
__export(broadcast_exports, {
  Consumer: () => Consumer4,
  Producer: () => Producer4
});

// vendor/moq/js/net/src/internal.ts
var hooks = {
  makeRequest: () => {
    throw new Error("track.ts not loaded");
  }
};

// vendor/moq/js/net/src/track.ts
var track_exports = {};
__export(track_exports, {
  Consumer: () => Consumer3,
  DEFAULT_LATENCY_MAX_MS: () => DEFAULT_LATENCY_MAX_MS,
  Producer: () => Producer3,
  Request: () => Request,
  Subscriber: () => Subscriber,
  infoDefaults: () => infoDefaults
});

// vendor/moq/js/net/src/group.ts
var group_exports = {};
__export(group_exports, {
  Consumer: () => Consumer2,
  Lagged: () => Lagged,
  MAX_GROUP_CACHE_BYTES: () => MAX_GROUP_CACHE_BYTES,
  MAX_GROUP_FRAMES: () => MAX_GROUP_FRAMES,
  Producer: () => Producer2
});

// vendor/moq/js/net/src/time.ts
var time_exports = {};
__export(time_exports, {
  Micro: () => Micro,
  Milli: () => Milli,
  Nano: () => Nano,
  Second: () => Second,
  Timescale: () => Timescale,
  Timestamp: () => Timestamp
});
var Nano = Object.assign((value) => value, {
  zero: 0,
  fromMicro: (us) => us * 1e3,
  fromMilli: (ms) => ms * 1e6,
  fromSecond: (s) => s * 1e9,
  toMicro: (ns) => ns / 1e3,
  toMilli: (ns) => ns / 1e6,
  toSecond: (ns) => ns / 1e9,
  now: () => performance.now() * 1e6,
  add: (a, b) => a + b,
  sub: (a, b) => a - b,
  mul: (a, b) => a * b,
  div: (a, b) => a / b,
  max: (a, b) => Math.max(a, b),
  min: (a, b) => Math.min(a, b)
});
var Micro = Object.assign((value) => value, {
  zero: 0,
  fromNano: (ns) => ns / 1e3,
  fromMilli: (ms) => ms * 1e3,
  fromSecond: (s) => s * 1e6,
  toNano: (us) => us * 1e3,
  toMilli: (us) => us / 1e3,
  toSecond: (us) => us / 1e6,
  now: () => performance.now() * 1e3,
  add: (a, b) => a + b,
  sub: (a, b) => a - b,
  mul: (a, b) => a * b,
  div: (a, b) => a / b,
  max: (a, b) => Math.max(a, b),
  min: (a, b) => Math.min(a, b)
});
var Milli = Object.assign((value) => value, {
  zero: 0,
  fromNano: (ns) => ns / 1e6,
  fromMicro: (us) => us / 1e3,
  fromSecond: (s) => s * 1e3,
  toNano: (ms) => ms * 1e6,
  toMicro: (ms) => ms * 1e3,
  toSecond: (ms) => ms / 1e3,
  now: () => performance.now(),
  add: (a, b) => a + b,
  sub: (a, b) => a - b,
  mul: (a, b) => a * b,
  div: (a, b) => a / b,
  max: (a, b) => Math.max(a, b),
  min: (a, b) => Math.min(a, b)
});
var Timescale = Object.assign(
  (unitsPerSecond) => {
    if (!Number.isInteger(unitsPerSecond) || unitsPerSecond <= 0) {
      throw new Error(`invalid timescale: ${unitsPerSecond}`);
    }
    return unitsPerSecond;
  },
  {
    /** One unit per second. */
    SECOND: 1,
    /** 1,000 units per second. */
    MILLI: 1e3,
    /** 1,000,000 units per second. */
    MICRO: 1e6,
    /** 1,000,000,000 units per second. */
    NANO: 1e9
  }
);
var Timestamp = class _Timestamp {
  /** The raw value, in `scale` units. */
  value;
  /** Units per second the {@link value} is measured in. */
  scale;
  /** Build a timestamp of `value` units at `scale`. */
  constructor(value, scale) {
    if (!Number.isFinite(value) || value < 0) {
      throw new Error(`invalid timestamp: ${value}`);
    }
    this.value = value;
    this.scale = scale;
  }
  /** Monotonic now (`performance.now()`, milliseconds since page load), not wall-clock time. */
  static now() {
    return new _Timestamp(performance.now(), Timescale.MILLI);
  }
  /** A timestamp of `ms` milliseconds. */
  static fromMillis(ms) {
    return new _Timestamp(ms, Timescale.MILLI);
  }
  /** A timestamp of `us` microseconds. */
  static fromMicros(us) {
    return new _Timestamp(us, Timescale.MICRO);
  }
  /** This timestamp's value re-expressed at `scale` (a raw number, not a new Timestamp). */
  as(scale) {
    return scale === this.scale ? this.value : this.value * scale / this.scale;
  }
  /** The value in milliseconds. */
  asMillis() {
    return this.as(Timescale.MILLI);
  }
  /** The value in microseconds. */
  asMicros() {
    return this.as(Timescale.MICRO);
  }
};
var Second = Object.assign((value) => value, {
  zero: 0,
  fromNano: (ns) => ns / 1e9,
  fromMicro: (us) => us / 1e6,
  fromMilli: (ms) => ms / 1e3,
  toNano: (s) => s * 1e9,
  toMicro: (s) => s * 1e6,
  toMilli: (s) => s * 1e3,
  now: () => performance.now() / 1e3,
  add: (a, b) => a + b,
  sub: (a, b) => a - b,
  mul: (a, b) => a * b,
  div: (a, b) => a / b,
  max: (a, b) => Math.max(a, b),
  min: (a, b) => Math.min(a, b)
});

// vendor/moq/js/net/src/group.ts
var MAX_GROUP_CACHE_BYTES = 32 * 1024 * 1024;
var MAX_GROUP_FRAMES = 1024;
var MAX_TIMER_DELAY_MS = 2147483647;
var Lagged = class extends Error {
  constructor() {
    super("lagged: frames were evicted before being read");
    this.name = "Lagged";
  }
};
var GroupState = class {
  sequence;
  frames = new Signal([]);
  closed = new Once();
  total = new Signal(0);
  // The total number of frames in the group thus far
  maxAgeMs;
  expiryTimer;
  evictedChanged;
  // Absolute sequence of the frame at the front of `frames`, advanced by a read and by an
  // eviction alike: the sequence the next read returns.
  start = 0;
  // One past the newest frame the cache cap evicted before it could be read, or 0 when
  // none was. A read that wanted a frame below it has a gap, so it throws Lagged rather
  // than skipping silently; a read that starts above it never asked for the missing
  // frames, so it proceeds.
  evicted = 0;
  cacheBytes = 0;
  framesChanged = /* @__PURE__ */ new Set();
  expired = /* @__PURE__ */ new Set();
  constructor(sequence, maxAgeMs) {
    this.sequence = sequence;
    this.maxAgeMs = maxAgeMs;
  }
};
function clearExpiryTimer(state) {
  if (state.expiryTimer === void 0) return;
  clearTimeout(state.expiryTimer);
  state.expiryTimer = void 0;
}
function scheduleExpiry(state) {
  if (state.maxAgeMs === void 0 || state.expiryTimer !== void 0) return;
  const first = state.frames.peek()[0];
  if (!first) return;
  const delay = Math.min(
    MAX_TIMER_DELAY_MS,
    Math.max(0, first.cachedAt + state.maxAgeMs - performance.now()) + 1
  );
  state.expiryTimer = setTimeout(() => {
    state.expiryTimer = void 0;
    pruneExpired(state);
  }, delay);
  const timer = state.expiryTimer;
  timer.unref?.();
}
function setMaxAgeMs(state, maxAgeMs) {
  state.maxAgeMs = maxAgeMs;
  clearExpiryTimer(state);
  pruneExpired(state);
}
function appendFrame(state, frame, cachedAt = performance.now()) {
  if (state.closed.peek() !== void 0) throw new Error("group is closed");
  pruneExpired(state);
  state.cacheBytes += frame.payload.byteLength;
  state.frames.mutate((frames) => {
    frames.push({ frame, cachedAt });
    while (frames.length > MAX_GROUP_FRAMES || state.cacheBytes > MAX_GROUP_CACHE_BYTES) {
      const evicted = frames.shift();
      if (!evicted) break;
      state.cacheBytes -= evicted.frame.payload.byteLength;
      state.start++;
      state.evicted = state.start;
      state.evictedChanged?.(true);
    }
  });
  state.total.update((total) => total + 1);
  for (const changed of state.framesChanged) changed();
  scheduleExpiry(state);
}
function evictFrames(state, count, markGap = true) {
  let dropped = 0;
  state.frames.mutate((frames) => {
    while (dropped < count) {
      const buffered = frames.shift();
      if (!buffered) break;
      state.cacheBytes -= buffered.frame.payload.byteLength;
      state.start++;
      if (markGap) state.evicted = state.start;
      dropped++;
    }
  });
  if (dropped > 0) state.evictedChanged?.(markGap);
  if (state.frames.peek().length === 0) clearExpiryTimer(state);
  else scheduleExpiry(state);
  return dropped;
}
function evictBefore(state, sequence, markGap = true) {
  return evictFrames(state, Math.max(0, sequence - state.start), markGap);
}
function pruneExpired(state, now = performance.now()) {
  if (state.maxAgeMs === void 0) return 0;
  const cutoff = now - state.maxAgeMs;
  let expired = 0;
  for (const buffered of state.frames.peek()) {
    if (buffered.cachedAt > cutoff) break;
    expired++;
  }
  const dropped = evictFrames(state, expired, false);
  if (dropped > 0) {
    for (const changed of state.expired) changed();
  }
  if (state.frames.peek().length > 0) scheduleExpiry(state);
  return dropped;
}
var Producer2 = class {
  /** Sequence number of this group within its track. */
  sequence;
  #state;
  #mirrors;
  // Whether any mirror reader is attached (see {@link used}). Fetch coalescing watches it to
  // cancel an abandoned download; a group can stay open indefinitely (a catalog or JSON stream),
  // so this is what stops a reader-less fetch instead of the stream ending on its own.
  #used = new Signal(false);
  constructor(sequence, maxAgeMs) {
    this.#state = new GroupState(sequence, maxAgeMs);
    this.#state.evictedChanged = (markGap) => {
      if (!this.#mirrors) return;
      for (const mirror of this.#mirrors) evictBefore(mirror, this.#state.start, markGap);
    };
    this.sequence = sequence;
  }
  /**
   * Settles once the group closes: `null` on a clean close, or the abort {@link Error}.
   * Peek it synchronously (`undefined` while open), observe it reactively, or `await` it.
   */
  get closed() {
    return this.#state.closed;
  }
  /** A read handle for this group. */
  consume() {
    return makeConsumer2(this.#state);
  }
  /**
   * Create an independent read handle that receives every frame written here.
   *
   * Frames written so far are replayed synchronously; later writes and close are teed
   * in as they happen.
   *
   * @internal Track fan-out and fetch coalescing only. Use {@link consume} instead.
   */
  mirror(maxAgeMs) {
    pruneExpired(this.#state);
    const dst = new GroupState(this.sequence, maxAgeMs);
    for (const buffered of this.#state.frames.peek()) appendFrame(dst, buffered.frame, buffered.cachedAt);
    dst.start = this.#state.start;
    dst.evicted = this.#state.evicted;
    dst.total.set(this.#state.total.peek());
    const closed = this.#state.closed.peek();
    if (closed !== void 0) {
      dst.closed.set(closed);
      return makeConsumer2(dst);
    }
    this.#mirrors ??= /* @__PURE__ */ new Set();
    this.#mirrors.add(dst);
    this.#used.set(true);
    const dispose = dst.closed.subscribe((c) => {
      if (c === void 0) return;
      this.#mirrors?.delete(dst);
      this.#used.set((this.#mirrors?.size ?? 0) > 0);
      dispose();
    });
    return makeConsumer2(dst);
  }
  /**
   * Whether any mirror reader is currently attached.
   *
   * Pairs with {@link unused}. Fetch coalescing watches it to cancel a download once every reader
   * has gone: a group can stay open indefinitely (a catalog track, a JSON stream), so it can't
   * rely on the stream ending on its own.
   *
   * @internal Track fan-out and fetch coalescing only.
   */
  get used() {
    return this.#used;
  }
  /** Number of frame payloads currently retained by this group. */
  get bufferedFrames() {
    pruneExpired(this.#state);
    return this.#state.frames.peek().length;
  }
  /**
   * Drop up to `count` oldest retained frames, and align every mirror to the
   * resulting source position. Track cache owners use this to enforce hard bounds.
   *
   * @internal
   */
  evictFrames(count, markGap = true) {
    return evictFrames(this.#state, count, markGap);
  }
  /** Apply or remove an absolute retained-frame age bound. @internal */
  setMaxAgeMs(maxAgeMs) {
    setMaxAgeMs(this.#state, maxAgeMs);
  }
  /** Observe retained-frame growth. Returns a function that removes the observer. @internal */
  onFramesChanged(changed) {
    this.#state.framesChanged.add(changed);
    return () => this.#state.framesChanged.delete(changed);
  }
  /** Observe age-based frame eviction. Returns a function that removes the observer. @internal */
  onExpired(changed) {
    this.#state.expired.add(changed);
    return () => this.#state.expired.delete(changed);
  }
  /**
   * Resolves once no mirror reader remains (or the group closes).
   *
   * @internal Track fan-out and fetch coalescing only.
   */
  async unused() {
    while (this.#used.peek() && this.#state.closed.peek() === void 0) {
      await Signal.race(this.#used, this.#state.closed);
    }
  }
  /** Writes a frame to the group. */
  writeFrame(frame) {
    const cachedAt = performance.now();
    appendFrame(this.#state, frame, cachedAt);
    if (this.#mirrors) {
      for (const mirror of this.#mirrors) {
        if (mirror.closed.peek() !== void 0) this.#mirrors.delete(mirror);
        else appendFrame(mirror, frame, cachedAt);
      }
    }
  }
  /** Write a string as a single UTF-8 encoded frame, stamped with {@link Timestamp.now}. */
  writeString(str) {
    this.writeFrame({ payload: new TextEncoder().encode(str), timestamp: Timestamp.now() });
  }
  /** Write a value as a single JSON-encoded frame, stamped with {@link Timestamp.now}. */
  writeJson(json) {
    this.writeString(JSON.stringify(json));
  }
  /** Write a boolean as a single one-byte frame, stamped with {@link Timestamp.now}. */
  writeBool(bool) {
    this.writeFrame({ payload: new Uint8Array([bool ? 1 : 0]), timestamp: Timestamp.now() });
  }
  /** True once the group has been closed. */
  get isClosed() {
    return this.#state.closed.peek() !== void 0;
  }
  /** Closes the group, optionally with an error to abort readers. */
  close(abort) {
    if (this.#state.closed.peek() !== void 0) return;
    this.#state.closed.set(abort ?? null);
    if (this.#mirrors) {
      for (const mirror of this.#mirrors) {
        if (mirror.closed.peek() === void 0) mirror.closed.set(abort ?? null);
      }
      this.#mirrors.clear();
    }
  }
};
var makeConsumer2;
var Consumer2 = class _Consumer {
  /** Sequence number of this group within its track. */
  sequence;
  #state;
  constructor(state) {
    this.#state = state;
    this.sequence = state.sequence;
  }
  /**
   * Settles once the group closes: `null` on a clean close, or the abort {@link Error}.
   * Peek it synchronously (`undefined` while open), observe it reactively, or `await` it.
   */
  get closed() {
    return this.#state.closed;
  }
  static {
    makeConsumer2 = (state) => new _Consumer(state);
  }
  #readBufferedFrame() {
    pruneExpired(this.#state);
    const frames = this.#state.frames.peek();
    const buffered = frames.shift();
    if (!buffered) return void 0;
    this.#state.cacheBytes -= buffered.frame.payload.byteLength;
    if (frames.length === 0) clearExpiryTimer(this.#state);
    return { sequence: this.#state.start++, frame: buffered.frame };
  }
  /** True once no further frames can be read: the group has closed and every buffered frame is read. */
  get done() {
    pruneExpired(this.#state);
    return this.#state.frames.peek().length === 0 && this.#state.closed.peek() !== void 0;
  }
  /** True once the group has been closed, regardless of whether buffered frames remain unread. Synchronous complement to the {@link closed} promise. */
  get isClosed() {
    return this.#state.closed.peek() !== void 0;
  }
  /** True if frames were evicted from the front of this group before being read. */
  get skipped() {
    return this.#state.evicted > 0;
  }
  /**
   * How many frames the group has held, including any already read or evicted.
   *
   * It is also the next frame's sequence number, so `frameCount - 1` names the newest
   * frame written so far. A publisher snapshots it to resolve the track's live edge.
   */
  get frameCount() {
    return this.#state.total.peek();
  }
  /** Number of frame payloads currently retained for this consumer. */
  get bufferedFrames() {
    pruneExpired(this.#state);
    return this.#state.frames.peek().length;
  }
  /** Drop up to `count` oldest retained frames. Track cache owners use this to enforce hard bounds. @internal */
  evictFrames(count, markGap = true) {
    return evictFrames(this.#state, count, markGap);
  }
  /** Apply or remove an absolute retained-frame age bound. @internal */
  setMaxAgeMs(maxAgeMs) {
    setMaxAgeMs(this.#state, maxAgeMs);
  }
  /** Observe retained-frame growth. Returns a function that removes the observer. @internal */
  onFramesChanged(changed) {
    this.#state.framesChanged.add(changed);
    return () => this.#state.framesChanged.delete(changed);
  }
  /**
   * Reads the next already-buffered frame without blocking.
   * Treat the returned frame bytes as read-only; they are shared with other consumers.
   *
   * Returns `undefined` when nothing is buffered right now. That is not by itself
   * end-of-group: check {@link done} to tell "no frame buffered yet" from "finished".
   */
  tryReadFrame() {
    const read = this.#readBufferedFrame();
    return read?.frame;
  }
  /** Like {@link tryReadFrame} but also reports the frame's sequence number within the group. */
  tryReadFrameSequence() {
    const read = this.#readBufferedFrame();
    if (!read) return void 0;
    return { sequence: read.sequence, payload: read.frame.payload, timestamp: read.frame.timestamp };
  }
  /** Resolves once {@link readFrame} would not block. */
  async readable() {
    for (; ; ) {
      pruneExpired(this.#state);
      if (this.#state.frames.peek().length > 0) return;
      if (this.#state.closed.peek() !== void 0) return;
      await Signal.race(this.#state.frames, this.#state.closed);
    }
  }
  /**
   * Reads the next frame from the group.
   * Treat the returned frame bytes as read-only; they are shared with other consumers.
   */
  async readFrame() {
    for (; ; ) {
      if (this.#state.evicted > 0) throw new Lagged();
      const read = this.#readBufferedFrame();
      if (read) return read.frame;
      const closed = this.#state.closed.peek();
      if (closed instanceof Error) throw closed;
      if (closed !== void 0) return;
      await Signal.race(this.#state.frames, this.#state.closed);
    }
  }
  /**
   * Reads the next frame along with its sequence number within the group.
   * Treat the returned frame bytes as read-only; they are shared with other consumers.
   */
  async readFrameSequence(options) {
    const from2 = options?.from ?? 0;
    for (; ; ) {
      if (this.#state.evicted > from2) throw new Lagged();
      const read = this.#readBufferedFrame();
      if (read) {
        if (read.sequence < from2) continue;
        return { sequence: read.sequence, payload: read.frame.payload, timestamp: read.frame.timestamp };
      }
      const closed = this.#state.closed.peek();
      if (closed instanceof Error) throw closed;
      if (closed !== void 0) return;
      await Signal.race(this.#state.frames, this.#state.closed);
    }
  }
  /** Reads the next frame and decodes its payload as a UTF-8 string. */
  async readString() {
    const frame = await this.readFrame();
    return frame ? new TextDecoder().decode(frame.payload) : void 0;
  }
  /** Reads the next frame and parses its payload as JSON. */
  async readJson() {
    const frame = await this.readString();
    return frame ? JSON.parse(frame) : void 0;
  }
  /** Reads the next frame and decodes its payload as a one-byte boolean. */
  async readBool() {
    const frame = await this.readFrame();
    return frame ? frame.payload[0] === 1 : void 0;
  }
  /** Closes the group, optionally with an error to abort readers. Idempotent. */
  close(abort) {
    if (this.#state.closed.peek() !== void 0) return;
    this.#state.closed.set(abort ?? null);
  }
};

// vendor/moq/js/net/src/track.ts
var DEFAULT_LATENCY_MAX_MS = 5e3;
function cacheCountLimit(name, value) {
  if (value === void 0) return void 0;
  if (!Number.isSafeInteger(value) || value < 1) {
    throw new RangeError(`${name} must be a positive safe integer: ${value}`);
  }
  return value;
}
function cacheAgeLimit(value) {
  if (value === void 0) return void 0;
  if (!Number.isFinite(value) || value <= 0) {
    throw new RangeError(`maxAgeMs must be a positive finite number: ${value}`);
  }
  return value;
}
function cacheLimits(limits = {}) {
  return {
    maxGroups: cacheCountLimit("maxGroups", limits.maxGroups),
    maxFrames: cacheCountLimit("maxFrames", limits.maxFrames),
    maxAgeMs: cacheAgeLimit(limits.maxAgeMs)
  };
}
var MAX_DATAGRAM_AGE_MS = 50;
var MAX_DATAGRAM_BYTES = 65535;
function latencyMaxMillis(value) {
  if (!Number.isFinite(value) || value < 0) {
    throw new RangeError(`latencyMax must be a non-negative number of milliseconds: ${value}`);
  }
  return Math.ceil(value);
}
function infoDefaults(info = {}) {
  return {
    timescale: info.timescale ?? Timescale.MILLI,
    latencyMax: latencyMaxMillis(info.latencyMax ?? DEFAULT_LATENCY_MAX_MS),
    priority: info.priority ?? 0,
    ordered: info.ordered ?? false
  };
}
function subscriptionDefaults(subscription = {}) {
  const limits = cacheLimits(subscription);
  return {
    priority: subscription.priority ?? 0,
    ordered: subscription.ordered ?? false,
    latencyMax: latencyMaxMillis(subscription.latencyMax ?? 0),
    startGroup: subscription.startGroup,
    endGroup: subscription.endGroup,
    maxGroups: limits.maxGroups,
    maxFrames: limits.maxFrames,
    maxAgeMs: limits.maxAgeMs
  };
}
function wireSubscription(subscription) {
  return {
    priority: subscription.priority,
    ordered: subscription.ordered,
    latencyMax: subscription.latencyMax,
    startGroup: subscription.startGroup,
    endGroup: subscription.endGroup
  };
}
function combineCacheLimits(base, states) {
  const combined = { ...base };
  for (const state of states) {
    const subscription = state.update.peek();
    if (!subscription) continue;
    for (const name of ["maxGroups", "maxFrames", "maxAgeMs"]) {
      const value = subscription[name];
      if (value === void 0) continue;
      combined[name] = combined[name] === void 0 ? value : Math.min(combined[name], value);
    }
  }
  return combined;
}
function combineSubscriptions(states) {
  let combined;
  for (const state of states) {
    const subscription = state.update.peek();
    if (!subscription) continue;
    if (!combined) {
      combined = wireSubscription(subscription);
      continue;
    }
    combined.priority = Math.max(combined.priority ?? 0, subscription.priority ?? 0);
    combined.ordered = (combined.ordered ?? false) && (subscription.ordered ?? false);
    combined.latencyMax = Math.max(combined.latencyMax ?? 0, subscription.latencyMax ?? 0);
    if (subscription.startGroup !== void 0) {
      combined.startGroup = combined.startGroup === void 0 ? subscription.startGroup : Math.min(combined.startGroup, subscription.startGroup);
    }
    if (combined.endGroup === void 0 || subscription.endGroup === void 0) {
      combined.endGroup = void 0;
    } else {
      combined.endGroup = Math.max(combined.endGroup, subscription.endGroup);
    }
  }
  return combined;
}
var Request = class _Request {
  /** The requested track name. */
  name;
  #producer;
  #sequences;
  #pending;
  constructor(options) {
    this.name = options.name;
    this.#producer = options.producer;
    this.#sequences = options.sequences;
    this.#pending = options.pending;
    this.#pending.add(this);
  }
  static {
    hooks.makeRequest = (options) => new _Request(options);
  }
  /** The aggregate subscription requested for this track. */
  get subscription() {
    return this.#producer.subscription.peek() ?? subscriptionDefaults();
  }
  /** The subscriber's priority for this track. */
  get priority() {
    return this.subscription.priority ?? 0;
  }
  /** Accept the request, committing the track's immutable {@link Info}. */
  accept(info = {}) {
    this.#pending.delete(this);
    bindProducer(this.name, this.#producer, this.#sequences);
    return this.#producer.accept(info);
  }
  /** Reject the request, closing the track optionally with an error. */
  reject(err) {
    this.#pending.delete(this);
    this.#producer.close(err);
  }
};
var Consumer3 = class {
  /** The track name. */
  name;
  #broadcast;
  constructor(name, broadcast) {
    this.name = name;
    this.#broadcast = broadcast;
  }
  /** Open a live subscription to the track. */
  subscribe(options) {
    return this.#broadcast.subscribe(this.name, options);
  }
  /** Fetch the track's immutable publisher properties without subscribing. */
  info() {
    return this.#broadcast.resolveTrackInfo(this.name);
  }
  /** Fetch a single group by sequence without holding a live subscription. */
  fetchGroup(sequence, options) {
    return this.#broadcast.fetchGroup(this.name, sequence, options);
  }
};
var TrackState = class {
  /** The producer fanning into this sink, so a subscriber can mint a sibling of itself. */
  producer;
  groups = new Signal([]);
  /** Best-effort datagram channel, parallel to {@link groups}; an age-evicted send buffer per subscriber. */
  datagrams = new Signal([]);
  latest;
  closed = new Once();
  update;
  /** Resolved once the producer commits the immutable properties. */
  info = new Signal(void 0);
  constructor(subscription) {
    this.update = new Signal(subscription === void 0 ? void 0 : subscriptionDefaults(subscription));
  }
};
function closeTrackState(state, abort) {
  if (state.closed.peek() !== void 0) return false;
  state.closed.set(abort ?? null);
  return true;
}
async function resolveInfo(state) {
  for (; ; ) {
    const info = state.info.peek();
    if (info) return info;
    const closed = state.closed.peek();
    if (closed instanceof Error) throw closed;
    if (closed !== void 0) throw new Error("track closed before info was known");
    await Signal.race(state.info, state.closed);
  }
}
function bindProducer(name, producer, sequences) {
  let shared = sequences.get(name);
  if (!shared) {
    shared = { next: 0 };
    sequences.set(name, shared);
  }
  bindProducerSequence(producer, shared);
}
var bindProducerSequence;
var pruneSubscriber;
var makeSubscriber;
var Producer3 = class {
  /** The track name. */
  name;
  // The producer's own state is the source of truth (info/closed); subscribers
  // read mirrored sinks, never this state directly.
  #state = new TrackState();
  #sequence = { next: 0 };
  #baseLimits;
  #limits;
  // Recently written source groups, retained for replay to late subscribers and
  // pruned once closed and older than the cache window. Each entry tracks the mirror
  // it handed to every sink so eviction can drop them too: otherwise a slow consumer
  // that never reads would pin old groups (and their frame bytes) forever.
  #cache = [];
  // One independent downstream state per live subscriber.
  #sinks = /* @__PURE__ */ new Set();
  // Whether any subscriber is currently attached. Exposed as {@link used}; the consumer wire
  // watches it to tear down an idle upstream, and a publisher can watch it for on-demand capture.
  #used = new Signal(false);
  constructor(name, limits = {}) {
    this.name = name;
    this.#baseLimits = cacheLimits(limits);
    this.#limits = this.#baseLimits;
  }
  static {
    bindProducerSequence = (producer, sequence) => {
      producer.#sequence = sequence;
    };
    pruneSubscriber = (state) => {
      const producer = state.producer;
      if (producer) {
        producer.#updateSubscription();
        producer.#pruneSink(state);
      }
    };
  }
  /**
   * Resolve this track's immutable publisher properties, committed at accept time.
   * Rejects if the track is closed before the properties are known.
   */
  info() {
    return resolveInfo(this.#state);
  }
  /**
   * Settles once the track closes: `null` on a clean close, or the abort {@link Error}.
   * Peek it synchronously (`undefined` while open), observe it reactively, or `await` it.
   */
  get closed() {
    return this.#state.closed;
  }
  /**
   * The aggregate subscription across live subscribers, or `undefined` when there are none.
   * The wire layer watches this to emit SUBSCRIBE_UPDATE.
   */
  get subscription() {
    return this.#state.update;
  }
  /** Commit the immutable publisher properties, resolving {@link info}. Returns `this`. */
  accept(info = {}) {
    if (this.#state.closed.peek() !== void 0) return this;
    const resolved = infoDefaults(info);
    this.#state.info.set(resolved);
    for (const sink of this.#sinks) sink.info.set(resolved);
    return this;
  }
  /** An independent {@link Subscriber} receiving a full copy of this track's groups. */
  subscribe(options = {}) {
    const sink = new TrackState(options);
    this.#addSink(sink);
    return makeSubscriber(this.name, sink);
  }
  /**
   * Whether the track currently has any subscribers.
   *
   * Watch it (`effect.get` / `.peek()`) to drive on-demand work: a publisher can start and stop
   * capture with demand, and the consumer wire watches it to tear an idle upstream subscription
   * down instead of downloading to nobody. Pairs with {@link unused}. Mirrors the Rust `Demand`.
   */
  get used() {
    return this.#used;
  }
  /** Resolves once the track has no subscribers (or has closed). Await it to react to demand ending. */
  async unused() {
    while (this.#used.peek() && this.#state.closed.peek() === void 0) {
      await Signal.race(this.#used, this.#state.closed);
    }
  }
  // Register a downstream sink: seed its info, replay the retained window, and (while
  // the track is open) mirror future groups into it. A late subscriber to a closed
  // track still drains the buffered groups before seeing the end.
  #addSink(sink) {
    sink.producer = this;
    const info = this.#state.info.peek();
    if (info) sink.info.set(info);
    const closed = this.#state.closed.peek();
    if (closed === void 0) {
      this.#sinks.add(sink);
      this.#used.set(true);
      const forward = sink.update.subscribe(() => this.#updateSubscription());
      this.#updateSubscription();
      const dispose = sink.closed.subscribe((c) => {
        if (c === void 0) return;
        const abort = c instanceof Error ? c : void 0;
        forward();
        this.#sinks.delete(sink);
        this.#updateSubscription();
        for (const entry of this.#cache) {
          const cached2 = entry.mirrors.get(sink);
          const mirror = cached2?.group;
          if (mirror) {
            cached2.dispose();
            mirror.evictFrames(Number.POSITIVE_INFINITY);
            mirror.close(abort);
            entry.mirrors.delete(sink);
          }
        }
        for (const group of sink.groups.peek()) group.close(abort);
        dispose();
        this.#used.set(this.#sinks.size > 0);
      });
    }
    this.#prune();
    for (const entry of this.#cache) this.#mirror(entry, sink);
    if (closed !== void 0) closeTrackState(sink, closed instanceof Error ? closed : void 0);
  }
  // Recompute from every live sink because an update or close can narrow as well as widen
  // the aggregate. The wire layer observes this signal and emits SUBSCRIBE_UPDATE.
  #updateSubscription() {
    this.#state.update.set(combineSubscriptions(this.#sinks));
    this.#limits = combineCacheLimits(this.#baseLimits, this.#sinks);
    for (const entry of this.#cache) entry.group.setMaxAgeMs(this.#limits.maxAgeMs);
    this.#prune();
    for (const sink of this.#sinks) this.#pruneSink(sink);
  }
  // A mirror cannot outlive its source, even when its own limit is omitted or
  // looser. A late sink on a closed track can still impose a tighter local bound.
  #sinkMaxAgeMs(sink) {
    const source = this.#limits.maxAgeMs;
    const local = sink.update.peek()?.maxAgeMs;
    return source === void 0 ? local : Math.min(source, local ?? source);
  }
  // Mirror a cached source group into a sink. The mirror fills synchronously as the
  // source is written and keeps its own read cursor; frame bytes are shared by
  // reference. Tracked on the entry so eviction can drop it from the sink.
  #mirror(entry, sink) {
    const dst = entry.group.mirror(this.#sinkMaxAgeMs(sink));
    const dispose = dst.onFramesChanged(() => this.#pruneSink(sink));
    entry.mirrors.set(sink, { group: dst, dispose });
    sink.latest = Math.max(sink.latest ?? 0, dst.sequence);
    sink.groups.mutate((groups) => {
      groups.push(dst);
      groups.sort((a, b) => a.sequence - b.sequence);
    });
    this.#pruneSink(sink);
  }
  #dropSinkGroup(sink, group) {
    sink.groups.mutate((groups) => {
      const index = groups.indexOf(group);
      if (index >= 0) groups.splice(index, 1);
    });
    for (const entry of this.#cache) {
      const cached2 = entry.mirrors.get(sink);
      if (cached2?.group !== group) continue;
      cached2.dispose();
      entry.mirrors.delete(sink);
      break;
    }
    group.evictFrames(Number.POSITIVE_INFINITY);
    group.close();
  }
  // Enforce local ownership limits synchronously after every group or frame append,
  // including mirrors already handed to a stalled reader.
  #pruneSink(sink) {
    const limits = sink.update.peek();
    const owned = this.#cache.flatMap((entry) => {
      const cached2 = entry.mirrors.get(sink);
      return cached2 ? [cached2.group] : [];
    });
    const maxAgeMs = this.#sinkMaxAgeMs(sink);
    for (const group of owned) group.setMaxAgeMs(maxAgeMs);
    while (limits?.maxGroups !== void 0 && owned.length > limits.maxGroups) {
      const group = owned.shift();
      if (group) this.#dropSinkGroup(sink, group);
    }
    if (limits?.maxFrames !== void 0) {
      let excess = owned.reduce((total, group) => total + group.bufferedFrames, 0) - limits.maxFrames;
      for (const group of owned) {
        if (excess <= 0) break;
        excess -= group.evictFrames(excess);
      }
    }
  }
  // Drop a cached group's mirror from every sink so no consumer can pin it.
  #evict(entry, markGap = true) {
    entry.dispose();
    entry.disposeExpired();
    for (const [sink, cached2] of entry.mirrors) {
      const mirror = cached2.group;
      sink.groups.mutate((groups) => {
        const i = groups.indexOf(mirror);
        if (i >= 0) groups.splice(i, 1);
      });
      cached2.dispose();
      mirror.evictFrames(Number.POSITIVE_INFINITY, markGap);
      mirror.close();
    }
    entry.mirrors.clear();
    entry.group.evictFrames(Number.POSITIVE_INFINITY, markGap);
    entry.group.close();
  }
  // Evict by age first, then enforce hard aggregate group and frame bounds.
  #prune() {
    const latencyMaxMs = this.#state.info.peek()?.latencyMax ?? DEFAULT_LATENCY_MAX_MS;
    const cutoff = performance.now() - latencyMaxMs;
    const retained = [];
    for (const entry of this.#cache) {
      const expired = this.#limits.maxAgeMs !== void 0 && entry.group.isClosed && entry.group.bufferedFrames === 0;
      if (!expired && (entry.time > cutoff || entry.group.closed.peek() === void 0)) {
        retained.push(entry);
        continue;
      }
      this.#evict(entry, !expired);
    }
    this.#cache = retained;
    while (this.#limits.maxGroups !== void 0 && this.#cache.length > this.#limits.maxGroups) {
      const entry = this.#cache.shift();
      if (entry) this.#evict(entry);
    }
    if (this.#limits.maxFrames !== void 0) {
      let excess = this.#cache.reduce((total, entry) => total + entry.group.bufferedFrames, 0) - this.#limits.maxFrames;
      for (const entry of this.#cache) {
        if (excess <= 0) break;
        excess -= entry.group.evictFrames(excess);
      }
    }
  }
  // Retain a source group and fan it out to every live sink.
  #publish(group) {
    const entry = {
      group,
      time: performance.now(),
      dispose: () => {
      },
      disposeExpired: () => {
      },
      mirrors: /* @__PURE__ */ new Map()
    };
    entry.dispose = group.onFramesChanged(() => this.#prune());
    entry.disposeExpired = group.onExpired(() => {
      queueMicrotask(() => {
        if (this.#cache.includes(entry)) this.#prune();
      });
    });
    this.#cache.push(entry);
    this.#prune();
    for (const sink of this.#sinks) this.#mirror(entry, sink);
  }
  /** Append a new group with the next sequence number. */
  appendGroup() {
    if (this.#state.closed.peek() !== void 0) throw new Error("track is closed");
    const sequence = this.#sequence;
    const group = new Producer2(sequence.next, this.#limits.maxAgeMs);
    sequence.next = group.sequence + 1;
    this.#publish(group);
    return group;
  }
  /**
   * Insert an existing group into the track.
   *
   * Throws on a sequence that is still cached: a live duplicate would fan out to every
   * subscriber twice. An aborted incarnation is evicted so a fresh group can serve the
   * sequence again. Best effort (mirrors Rust): nothing remembers a sequence whose cache
   * entry is already gone, so a long-evicted sequence is accepted as new.
   */
  writeGroup(group) {
    if (this.#state.closed.peek() !== void 0) throw new Error("track is closed");
    group.setMaxAgeMs(this.#limits.maxAgeMs);
    const existing = this.#cache.findIndex((entry) => entry.group.sequence === group.sequence);
    if (existing >= 0) {
      const entry = this.#cache[existing];
      if (!(entry.group.closed.peek() instanceof Error)) {
        throw new Error(`duplicate group: sequence=${group.sequence}`);
      }
      this.#evict(entry);
      this.#cache.splice(existing, 1);
    }
    const sequence = this.#sequence;
    if (group.sequence >= sequence.next) {
      sequence.next = group.sequence + 1;
    }
    this.#publish(group);
  }
  // Fan a datagram out to every live subscriber, dropping the oldest once the ring is full.
  // Late subscribers do NOT replay old datagrams (best-effort, unlike the group cache).
  #publishDatagram(datagram) {
    const now = performance.now();
    for (const sink of this.#sinks) {
      sink.datagrams.mutate((list) => {
        list.push({ datagram, time: now });
        while (list.length > 0 && now - list[0].time > MAX_DATAGRAM_AGE_MS) list.shift();
      });
    }
  }
  /**
   * Append a datagram with the next sequence number, returning the assigned sequence.
   *
   * A datagram is delivered best-effort over a single QUIC datagram, parallel to the track's
   * groups but drawing from the same sequence namespace (interleaving with {@link appendGroup}
   * never reuses a number). The payload must fit the negotiated transport datagram size minus
   * a small header; an oversize payload is dropped at each hop (there is no group fallback), so
   * keep datagram payloads small (e.g. a single audio frame). Datagrams are never delivered
   * over IETF moq-transport or stream-only transports (the WebSocket fallback). A payload over
   * 65535 bytes (the QUIC datagram frame ceiling) throws. An origin publisher uses this; a
   * relay preserving upstream numbering uses {@link writeDatagram}.
   */
  appendDatagram(timestamp, payload) {
    if (this.#state.closed.peek() !== void 0) throw new Error("track is closed");
    if (payload.byteLength > MAX_DATAGRAM_BYTES) throw new Error("datagram payload too large");
    const counter = this.#sequence;
    const sequence = counter.next;
    counter.next = sequence + 1;
    this.#publishDatagram({ sequence, timestamp, payload });
    return sequence;
  }
  /**
   * Write a datagram with an explicit sequence number.
   *
   * Preserves the supplied sequence (advancing the shared counter if needed) so a relay can
   * forward a datagram without renumbering it. The size limits of {@link appendDatagram}
   * apply. Most origin publishers want {@link appendDatagram} instead.
   */
  writeDatagram(datagram) {
    if (this.#state.closed.peek() !== void 0) throw new Error("track is closed");
    if (datagram.payload.byteLength > MAX_DATAGRAM_BYTES) throw new Error("datagram payload too large");
    const sequence = this.#sequence;
    if (datagram.sequence >= sequence.next) {
      sequence.next = datagram.sequence + 1;
    }
    this.#publishDatagram(datagram);
  }
  /** Close the track and every subscriber, mirroring the abort to their groups. Idempotent. */
  close(abort) {
    closeTrackState(this.#state, abort);
    for (const { group } of this.#cache) group.close(abort);
    for (const sink of this.#sinks) {
      for (const group of sink.groups.peek()) group.close(abort);
      closeTrackState(sink, abort);
    }
    this.#sinks.clear();
  }
  /** Append a frame as its own single-frame group. */
  writeFrame(frame) {
    const group = this.appendGroup();
    group.writeFrame(frame);
    group.close();
  }
  /** Appends a string to the track as its own single-frame group. */
  writeString(str) {
    const group = this.appendGroup();
    group.writeString(str);
    group.close();
  }
  /** Appends a JSON value to the track as its own single-frame group. */
  writeJson(json) {
    const group = this.appendGroup();
    group.writeJson(json);
    group.close();
  }
  /** Appends a boolean to the track as its own single-frame group. */
  writeBool(bool) {
    const group = this.appendGroup();
    group.writeBool(bool);
    group.close();
  }
};
var Subscriber = class _Subscriber {
  /** The track name. */
  name;
  #state;
  #nextSequence = 0;
  #cursor = new Signal({ start: 0 });
  constructor(name, state) {
    this.name = name;
    this.#state = state;
  }
  static {
    makeSubscriber = (name, state) => new _Subscriber(name, state);
  }
  /**
   * Resolve this track's immutable publisher properties.
   *
   * Resolves once the wire layer commits the TRACK_INFO it received (lite-05+) or
   * defaults (older drafts), so awaiting it never yields a placeholder. Rejects if
   * the track is closed before the properties are known (e.g. a rejected subscription).
   */
  info() {
    return resolveInfo(this.#state);
  }
  /** Settles once the track closes; see {@link Producer.closed}. */
  get closed() {
    return this.#state.closed;
  }
  /** This subscriber's current options, including defaults and the last {@link update}. */
  get subscription() {
    return this.#state.update;
  }
  /** Return the latest group sequence observed on this track, if any. */
  latest() {
    return this.#state.latest;
  }
  /**
   * The newest frame this track has produced, or `undefined` while it has none.
   *
   * Read from the groups buffered for this subscriber, so nothing is consumed. A newest
   * group that has no frames yet falls back to the newest older group that does. It
   * reads as `undefined` once the newest group has been evicted or received: what is
   * left can no longer say where the edge is.
   */
  largest() {
    const latest = this.#state.latest;
    if (latest === void 0) return void 0;
    const groups = this.#state.groups.peek();
    const newest = groups[groups.length - 1];
    if (!newest || newest.sequence !== latest) return void 0;
    for (let i = groups.length - 1; i >= 0; i--) {
      const count = groups[i].frameCount;
      if (count > 0) return { group: groups[i].sequence, frame: count - 1 };
    }
    return void 0;
  }
  /**
   * An independent subscriber to the same track, with its own cursor and its own replay of
   * the retained window.
   *
   * Reads here do not consume anything this one would deliver, which is what lets a
   * publisher read the cache alongside the cursor it is serving from. Resolving the track
   * again through the broadcast would not do: a dynamic serve is one request per peer
   * subscription, so that mints a second producer the application has to answer separately.
   *
   * @internal Wire layers only.
   */
  fork(options) {
    const producer = this.#state.producer;
    if (!producer) throw new Error("track has no producer to fork from");
    return producer.subscribe(options);
  }
  /** Start this subscriber's local read cursor at `sequence`, without changing its wire request. */
  startAt(sequence) {
    this.#cursor.update((cursor) => ({ ...cursor, start: sequence }));
  }
  /**
   * Cap {@link nextGroup} and {@link recvGroup} at `sequence` inclusively, or omit it to
   * remove the cap. Groups above the cap remain buffered and become readable if the cap
   * is raised. This local cursor does not change the subscription's wire request.
   */
  endAt(sequence) {
    this.#cursor.update((cursor) => ({ ...cursor, end: sequence }));
  }
  /** Close the track (optionally with an error), closing any pending groups. Idempotent. */
  close(abort) {
    closeTrackState(this.#state, abort);
    this.#state.groups.mutate((groups) => {
      for (const group of groups) group.close(abort);
      groups.length = 0;
    });
  }
  /**
   * Receive every group on this track exactly once, as it becomes available.
   *
   * Groups may arrive out of order or with gaps due to network conditions; unlike
   * {@link nextGroup}, one that arrives after a newer group was already returned is
   * still delivered. When several groups are buffered, the lowest sequence is
   * returned first.
   *
   * Honors the floor set by {@link startAt} and the cap set by {@link endAt}: a group
   * beyond the cap stays buffered (not dropped) and is offered once the cap rises, even
   * after a clean close, without blocking in-range groups that arrive behind it.
   */
  async recvGroup() {
    for (; ; ) {
      const group = this.tryRecvGroup();
      if (group) return group;
      const closed = this.#state.closed.peek();
      if (closed instanceof Error) throw closed;
      if (closed !== void 0 && !this.#state.groups.peek()[0]) return void 0;
      await Signal.race(this.#state.groups, this.#cursor, this.#state.closed);
    }
  }
  /**
   * Take the next buffered group without blocking, honoring the same cursor bounds as
   * {@link recvGroup}.
   *
   * Returns `undefined` when nothing is deliverable right now, which is not by itself the
   * end of the track: a group may still arrive, or one may be parked beyond the
   * {@link endAt} cap. Use it to drain what the retained window already holds, where
   * waiting for a sequence nothing will republish would park forever.
   */
  tryRecvGroup() {
    const groups = this.#state.groups.peek();
    const { start, end } = this.#cursor.peek();
    while (groups.length > 0 && groups[0].sequence < start) groups.shift()?.close();
    const group = groups[0];
    if (group && (end === void 0 || group.sequence <= end)) {
      groups.shift();
      return group;
    }
    return void 0;
  }
  /**
   * Receive the next datagram in arrival order.
   *
   * Datagrams are a separate best-effort channel from groups (see
   * {@link Producer.appendDatagram}); they share only the sequence namespace. A consumer
   * that falls too far behind silently loses the oldest datagrams. Read this alongside
   * {@link recvGroup} (e.g. in a separate loop) to receive both channels concurrently. Returning
   * a datagram advances {@link nextGroup} past that sequence.
   */
  async recvDatagram() {
    for (; ; ) {
      const datagrams2 = this.#state.datagrams.peek();
      const cutoff = performance.now() - MAX_DATAGRAM_AGE_MS;
      while (datagrams2.length > 0 && datagrams2[0].time < cutoff) datagrams2.shift();
      if (datagrams2.length > 0) {
        const datagram = datagrams2.shift()?.datagram;
        if (datagram) {
          this.#nextSequence = Math.max(this.#nextSequence, datagram.sequence + 1);
        }
        return datagram;
      }
      const closed = this.#state.closed.peek();
      if (closed instanceof Error) throw closed;
      if (closed !== void 0) return void 0;
      await Signal.race(this.#state.datagrams, this.#state.closed);
    }
  }
  /**
   * Return the next group with a strictly-greater sequence number than the last returned.
   *
   * Late arrivals (sequence at or below the last returned) are silently skipped.
   * Use {@link recvGroup} to see every group in arrival order instead.
   */
  async nextGroup() {
    for (; ; ) {
      const groups = this.#state.groups.peek();
      const cursor = this.#cursor.peek();
      const start = Math.max(cursor.start, this.#nextSequence);
      while (groups.length > 0 && groups[0].sequence < start) groups.shift()?.close();
      const group = groups[0];
      if (group && (cursor.end === void 0 || group.sequence <= cursor.end)) {
        groups.shift();
        this.#nextSequence = group.sequence + 1;
        return group;
      }
      const closed = this.#state.closed.peek();
      if (closed instanceof Error) throw closed;
      if (closed !== void 0 && !group) return void 0;
      await Signal.race(this.#state.groups, this.#cursor, this.#state.closed);
    }
  }
  /**
   * Reads the next frame across all groups, discarding older groups.
   * Treat the returned frame bytes as read-only; they are shared with other consumers.
   */
  async readFrame() {
    const next = await this.readFrameSequence();
    return next ? { payload: next.payload, timestamp: next.timestamp } : void 0;
  }
  /**
   * Reads the next frame along with its group and frame sequence numbers.
   * Treat the returned frame bytes as read-only; they are shared with other consumers.
   */
  async readFrameSequence() {
    for (; ; ) {
      const groups = this.#state.groups.peek();
      const { start } = this.#cursor.peek();
      while (groups.length > 0 && groups[0].sequence < start) groups.shift()?.close();
      while (groups.length > 1) {
        if (groups[0].skipped) {
          groups.shift()?.close();
          throw new Lagged();
        }
        const next2 = groups[0].tryReadFrameSequence();
        if (next2) {
          return {
            group: groups[0].sequence,
            frame: next2.sequence,
            payload: next2.payload,
            timestamp: next2.timestamp
          };
        }
        groups.shift()?.close();
      }
      if (groups.length === 0) {
        const closed2 = this.#state.closed.peek();
        if (closed2 instanceof Error) throw closed2;
        if (closed2 !== void 0) return void 0;
        await Signal.race(this.#state.groups, this.#cursor, this.#state.closed);
        continue;
      }
      const group = groups[0];
      if (group.skipped) {
        groups.shift()?.close();
        throw new Lagged();
      }
      const next = group.tryReadFrameSequence();
      if (next)
        return {
          group: group.sequence,
          frame: next.sequence,
          payload: next.payload,
          timestamp: next.timestamp
        };
      const closed = this.#state.closed.peek();
      if (closed instanceof Error) throw closed;
      if (closed !== void 0) return void 0;
      if (group.done) {
        groups.shift()?.close();
        continue;
      }
      await Promise.race([Signal.race(this.#state.groups, this.#cursor, this.#state.closed), group.readable()]);
    }
  }
  /** Reads the next frame and decodes it as a UTF-8 string. */
  async readString() {
    const next = await this.readFrame();
    if (!next) return void 0;
    return new TextDecoder().decode(next.payload);
  }
  /** Reads the next frame and parses it as JSON. */
  async readJson() {
    const next = await this.readString();
    if (!next) return void 0;
    return JSON.parse(next);
  }
  /** Reads the next frame and decodes it as a one-byte boolean, throwing on a malformed frame. */
  async readBool() {
    const next = await this.readFrame();
    if (!next) return void 0;
    const payload = next.payload;
    if (payload.byteLength !== 1 || !(payload[0] === 0 || payload[0] === 1)) throw new Error("invalid bool frame");
    return payload[0] === 1;
  }
  /**
   * Update this subscription's options (e.g. priority), triggering a SUBSCRIBE_UPDATE to the
   * publisher. Mirrors the Rust `Subscriber::update`.
   */
  update(options) {
    this.#state.update.set(subscriptionDefaults(options));
    pruneSubscriber(this.#state);
  }
};

// vendor/moq/js/net/src/broadcast.ts
var BroadcastState = class {
  requested = new Signal([]);
  pending = /* @__PURE__ */ new Set();
  closed = new Once();
  tracks = /* @__PURE__ */ new Map();
  sequences = /* @__PURE__ */ new Map();
  // Live consumer handles sharing this state (see {@link Consumer.clone}). The broadcast
  // closes once the last one closes, so a shared consumer can be handed to several callers.
  consumers = 0;
};
function dequeueRequest(state) {
  const requested = state.requested.peek();
  requested.sort((a, b) => a.priority - b.priority);
  return requested.pop();
}
function closeState2(state, abort) {
  if (state.closed.peek() !== void 0) return;
  state.closed.set(abort ?? null);
  for (const request of state.pending) request.reject(abort);
  state.requested.mutate((requests) => {
    requests.length = 0;
  });
}
function subscribe(state, name, options = {}, register = false) {
  if (state.closed.peek() !== void 0) {
    throw new Error(`broadcast is closed: ${state.closed.peek()}`);
  }
  const existing = state.tracks.get(name);
  if (existing) {
    if (existing.closed.peek() === void 0) return existing.subscribe(options);
    state.tracks.delete(name);
  }
  const producer = new Producer3(name);
  const subscriber = producer.subscribe(options);
  if (register) {
    state.tracks.set(name, producer);
    void producer.closed.then(() => {
      if (state.tracks.get(name) === producer) state.tracks.delete(name);
    });
  }
  state.requested.mutate((requested) => {
    requested.push(hooks.makeRequest({ name, producer, sequences: state.sequences, pending: state.pending }));
  });
  return subscriber;
}
async function resolveTrackInfo(state, name) {
  const existing = state.tracks.get(name);
  if (existing && existing.closed.peek() === void 0) {
    return existing.info();
  }
  if (state.closed.peek() !== void 0) {
    return Promise.reject(new Error(`broadcast is closed: ${state.closed.peek()}`));
  }
  const producer = new Producer3(name);
  state.requested.mutate((requested) => {
    requested.push(hooks.makeRequest({ name, producer, sequences: state.sequences, pending: state.pending }));
  });
  try {
    return await producer.info();
  } finally {
    producer.close();
  }
}
async function fetchGroup(state, name, sequence, options = {}) {
  const subscriber = subscribe(state, name, { priority: options.priority });
  try {
    for (; ; ) {
      const group = await subscriber.recvGroup();
      if (!group) throw new Error(`group not found: ${sequence}`);
      if (group.sequence === sequence) {
        void group.closed.then(() => subscriber.close());
        return group;
      }
      group.close();
      if (group.sequence > sequence) throw new Error(`group not found: ${sequence}`);
    }
  } catch (err) {
    subscriber.close();
    throw err;
  }
}
var Producer4 = class {
  #state = new BroadcastState();
  /**
   * Settles once the broadcast closes: `null` on a clean close, or the abort {@link Error}.
   * Peek it synchronously (`undefined` while open), observe it reactively, or `await` it.
   */
  get closed() {
    return this.#state.closed;
  }
  /** A read handle for this broadcast. */
  consume() {
    return makeConsumer3(this.#state);
  }
  /** Return the next track requested by a peer. */
  async requested() {
    for (; ; ) {
      const request = dequeueRequest(this.#state);
      if (request) return request;
      const closed = this.#state.closed.peek();
      if (closed instanceof Error) throw closed;
      if (closed !== void 0) return void 0;
      await Signal.race(this.#state.requested, this.#state.closed);
    }
  }
  /** Insert a track that is served directly, without an on-demand request round-trip. */
  insertTrack(track) {
    if (this.#state.closed.peek() !== void 0) {
      throw new Error(`broadcast is closed: ${this.#state.closed.peek()}`);
    }
    const existing = this.#state.tracks.get(track.name);
    if (existing && existing.closed.peek() === void 0) {
      throw new Error(`duplicate track: ${track.name}`);
    }
    this.#state.tracks.set(track.name, track);
    void track.closed.then(() => {
      if (this.#state.tracks.get(track.name) === track) {
        this.#state.tracks.delete(track.name);
      }
    });
  }
  /** Create a track, insert it into the broadcast, and return its producer. */
  createTrack(name, info = {}, limits = {}) {
    const producer = new Producer3(name, limits).accept(info);
    this.insertTrack(producer);
    return producer;
  }
  /** Remove a statically inserted track by name. */
  removeTrack(name) {
    this.#state.tracks.delete(name);
  }
  /** Open a live subscription to a track. Used by the publishing wire layer. */
  subscribe(name, options) {
    return subscribe(this.#state, name, options);
  }
  /** Resolve a track's immutable info. Used by the publishing wire layer. */
  resolveTrackInfo(name) {
    return resolveTrackInfo(this.#state, name);
  }
  /** Fetch a single group from the local retained window. Used by track handles. */
  fetchGroup(name, sequence, options) {
    return fetchGroup(this.#state, name, sequence, options);
  }
  /** A lazy read handle for a track on this broadcast. */
  track(name) {
    return new Consumer3(name, this);
  }
  /** Close the broadcast, optionally with an error to abort waiters. Idempotent. */
  close(abort) {
    closeState2(this.#state, abort);
  }
};
var makeConsumer3;
var Consumer4 = class _Consumer {
  #state;
  // Guards against a double close() on this handle over-decrementing the consumer count.
  #closed = false;
  constructor(state) {
    this.#state = state ?? new BroadcastState();
    this.#state.consumers++;
  }
  static {
    makeConsumer3 = (state) => new _Consumer(state);
  }
  /**
   * Settles once the broadcast closes: `null` on a clean close, or the abort {@link Error}.
   * Peek it synchronously (`undefined` while open), observe it reactively, or `await` it.
   *
   * Shared by every {@link clone}: it settles once the last handle closes. The subscribing
   * wire layer peeks it to evict a closed entry from its per-path consume cache.
   */
  get closed() {
    return this.#state.closed;
  }
  /**
   * Return another handle to the same broadcast, reference-counted with this one.
   *
   * Both handles read the same tracks and share one {@link closed} state; the broadcast
   * closes only once *every* handle has {@link close}d. Used by the connection's per-path
   * consume cache to share one subscription across callers. Subclasses that resolve info over
   * the wire override this to preserve their type (see the wire layer's consumed broadcast).
   */
  clone() {
    return new _Consumer(this.shareState());
  }
  // Hand this consumer's backing state to a clone. Opaque (`never`) so the state type stays
  // unexported; a subclass passes it straight back into its own `super(...)`.
  shareState() {
    return this.#state;
  }
  /** Get a lazy handle for a track on this broadcast. Repeat subscriptions dedupe onto one upstream subscription. */
  track(name) {
    return new Consumer3(name, this);
  }
  /** Open a live subscription to a track. Used by the subscribing wire layer. Repeat subscriptions to the same track share one upstream subscription. */
  subscribe(name, options) {
    return subscribe(this.#state, name, options, true);
  }
  /** Return the next track requested by the local consumer. Used by the subscribing wire layer. */
  async requested() {
    for (; ; ) {
      const request = dequeueRequest(this.#state);
      if (request) return request;
      const closed = this.#state.closed.peek();
      if (closed instanceof Error) throw closed;
      if (closed !== void 0) return void 0;
      await Signal.race(this.#state.requested, this.#state.closed);
    }
  }
  /**
   * Resolve a track's immutable info. Used by track handles. This base resolves it from
   * the local producers; the consuming wire layer overrides it to fetch over the wire.
   */
  resolveTrackInfo(name) {
    return resolveTrackInfo(this.#state, name);
  }
  /**
   * Fetch a single group by sequence. Used by track handles. This base serves from the
   * local retained window; the consuming wire layer overrides it to fetch over the wire
   * (or to reject when the transport has no FETCH).
   */
  fetchGroup(name, sequence, options) {
    return fetchGroup(this.#state, name, sequence, options);
  }
  /**
   * Release this handle. The broadcast is closed (optionally with an error to abort waiters)
   * once this was the last live handle; while other {@link clone}s remain open it stays live.
   */
  close(abort) {
    if (this.#closed) return;
    this.#closed = true;
    if (--this.#state.consumers > 0) return;
    closeState2(this.#state, abort);
  }
};

// vendor/moq/js/net/src/connection/index.ts
var connection_exports = {};
__export(connection_exports, {
  Reload: () => Reload,
  accept: () => accept,
  certificateHash: () => certificateHash,
  connect: () => connect,
  isWebTransportSupported: () => isWebTransportSupported
});

// scripts/moq-browser/node_modules/async-mutex/index.mjs
var E_TIMEOUT = new Error("timeout while waiting for mutex to become available");
var E_ALREADY_LOCKED = new Error("mutex already locked");
var E_CANCELED = new Error("request for lock canceled");
var __awaiter$2 = function(thisArg, _arguments, P, generator) {
  function adopt(value) {
    return value instanceof P ? value : new P(function(resolve2) {
      resolve2(value);
    });
  }
  return new (P || (P = Promise))(function(resolve2, reject) {
    function fulfilled(value) {
      try {
        step(generator.next(value));
      } catch (e) {
        reject(e);
      }
    }
    function rejected(value) {
      try {
        step(generator["throw"](value));
      } catch (e) {
        reject(e);
      }
    }
    function step(result) {
      result.done ? resolve2(result.value) : adopt(result.value).then(fulfilled, rejected);
    }
    step((generator = generator.apply(thisArg, _arguments || [])).next());
  });
};
var Semaphore = class {
  constructor(_value, _cancelError = E_CANCELED) {
    this._value = _value;
    this._cancelError = _cancelError;
    this._queue = [];
    this._weightedWaiters = [];
  }
  acquire(weight = 1, priority = 0) {
    if (weight <= 0)
      throw new Error(`invalid weight ${weight}: must be positive`);
    return new Promise((resolve2, reject) => {
      const task = { resolve: resolve2, reject, weight, priority };
      const i = findIndexFromEnd(this._queue, (other) => priority <= other.priority);
      if (i === -1 && weight <= this._value) {
        this._dispatchItem(task);
      } else {
        this._queue.splice(i + 1, 0, task);
      }
    });
  }
  runExclusive(callback_1) {
    return __awaiter$2(this, arguments, void 0, function* (callback, weight = 1, priority = 0) {
      const [value, release] = yield this.acquire(weight, priority);
      try {
        return yield callback(value);
      } finally {
        release();
      }
    });
  }
  waitForUnlock(weight = 1, priority = 0) {
    if (weight <= 0)
      throw new Error(`invalid weight ${weight}: must be positive`);
    if (this._couldLockImmediately(weight, priority)) {
      return Promise.resolve();
    } else {
      return new Promise((resolve2) => {
        if (!this._weightedWaiters[weight - 1])
          this._weightedWaiters[weight - 1] = [];
        insertSorted(this._weightedWaiters[weight - 1], { resolve: resolve2, priority });
      });
    }
  }
  isLocked() {
    return this._value <= 0;
  }
  getValue() {
    return this._value;
  }
  setValue(value) {
    this._value = value;
    this._dispatchQueue();
  }
  release(weight = 1) {
    if (weight <= 0)
      throw new Error(`invalid weight ${weight}: must be positive`);
    this._value += weight;
    this._dispatchQueue();
  }
  cancel() {
    this._queue.forEach((entry) => entry.reject(this._cancelError));
    this._queue = [];
  }
  _dispatchQueue() {
    this._drainUnlockWaiters();
    while (this._queue.length > 0 && this._queue[0].weight <= this._value) {
      this._dispatchItem(this._queue.shift());
      this._drainUnlockWaiters();
    }
  }
  _dispatchItem(item) {
    const previousValue = this._value;
    this._value -= item.weight;
    item.resolve([previousValue, this._newReleaser(item.weight)]);
  }
  _newReleaser(weight) {
    let called = false;
    return () => {
      if (called)
        return;
      called = true;
      this.release(weight);
    };
  }
  _drainUnlockWaiters() {
    if (this._queue.length === 0) {
      for (let weight = this._value; weight > 0; weight--) {
        const waiters = this._weightedWaiters[weight - 1];
        if (!waiters)
          continue;
        waiters.forEach((waiter) => waiter.resolve());
        this._weightedWaiters[weight - 1] = [];
      }
    } else {
      const queuedPriority = this._queue[0].priority;
      for (let weight = this._value; weight > 0; weight--) {
        const waiters = this._weightedWaiters[weight - 1];
        if (!waiters)
          continue;
        const i = waiters.findIndex((waiter) => waiter.priority <= queuedPriority);
        (i === -1 ? waiters : waiters.splice(0, i)).forEach(((waiter) => waiter.resolve()));
      }
    }
  }
  _couldLockImmediately(weight, priority) {
    return (this._queue.length === 0 || this._queue[0].priority < priority) && weight <= this._value;
  }
};
function insertSorted(a, v) {
  const i = findIndexFromEnd(a, (other) => v.priority <= other.priority);
  a.splice(i + 1, 0, v);
}
function findIndexFromEnd(a, predicate) {
  for (let i = a.length - 1; i >= 0; i--) {
    if (predicate(a[i])) {
      return i;
    }
  }
  return -1;
}
var __awaiter$1 = function(thisArg, _arguments, P, generator) {
  function adopt(value) {
    return value instanceof P ? value : new P(function(resolve2) {
      resolve2(value);
    });
  }
  return new (P || (P = Promise))(function(resolve2, reject) {
    function fulfilled(value) {
      try {
        step(generator.next(value));
      } catch (e) {
        reject(e);
      }
    }
    function rejected(value) {
      try {
        step(generator["throw"](value));
      } catch (e) {
        reject(e);
      }
    }
    function step(result) {
      result.done ? resolve2(result.value) : adopt(result.value).then(fulfilled, rejected);
    }
    step((generator = generator.apply(thisArg, _arguments || [])).next());
  });
};
var Mutex = class {
  constructor(cancelError) {
    this._semaphore = new Semaphore(1, cancelError);
  }
  acquire() {
    return __awaiter$1(this, arguments, void 0, function* (priority = 0) {
      const [, releaser] = yield this._semaphore.acquire(1, priority);
      return releaser;
    });
  }
  runExclusive(callback, priority = 0) {
    return this._semaphore.runExclusive(() => callback(), 1, priority);
  }
  isLocked() {
    return this._semaphore.isLocked();
  }
  waitForUnlock(priority = 0) {
    return this._semaphore.waitForUnlock(1, priority);
  }
  release() {
    if (this._semaphore.isLocked())
      this._semaphore.release();
  }
  cancel() {
    return this._semaphore.cancel();
  }
};

// vendor/moq/js/net/src/error.ts
var RemoteError = class extends Error {
  /** The code the peer sent, verbatim. */
  code;
  constructor(code, options) {
    super(`remote error: ${code}`, options);
    this.name = "RemoteError";
    this.code = code;
  }
};
var ProtocolViolation = class extends Error {
  constructor(message, options) {
    super(message, options);
    this.name = "ProtocolViolation";
  }
};
function streamCode(err) {
  if (typeof err !== "object" || err === null) return void 0;
  const { source, streamErrorCode } = err;
  if (source !== "stream" || typeof streamErrorCode !== "number") return void 0;
  return streamErrorCode;
}
function fromTransport(err) {
  const code = streamCode(err);
  if (code === void 0) return error(err);
  return new RemoteError(code, { cause: err });
}
function error(err) {
  return err instanceof Error ? err : new Error(String(err));
}
function reason(err) {
  const e = error(err);
  if (typeof WebTransportError !== "undefined" && e instanceof WebTransportError) {
    const parts2 = [`source=${e.source}`];
    if (e.streamErrorCode !== null) parts2.push(`code=${e.streamErrorCode}`);
    const detail = parts2.join(" ");
    return e.message ? `${e.message} (${detail})` : `WebTransportError: ${detail}`;
  }
  return e.message || e.name || "unknown error";
}

// vendor/moq/js/net/src/ietf/version.ts
var Version = {
  /**
   * draft-ietf-moq-transport-07
   * https://www.ietf.org/archive/id/draft-ietf-moq-transport-07.txt
   */
  DRAFT_07: 4278190087,
  /**
   * draft-ietf-moq-transport-14
   * https://www.ietf.org/archive/id/draft-ietf-moq-transport-14.txt
   */
  DRAFT_14: 4278190094,
  /**
   * draft-ietf-moq-transport-15
   * https://www.ietf.org/archive/id/draft-ietf-moq-transport-15.txt
   */
  DRAFT_15: 4278190095,
  /**
   * draft-ietf-moq-transport-16
   * https://www.ietf.org/archive/id/draft-ietf-moq-transport-16.txt
   */
  DRAFT_16: 4278190096,
  /**
   * draft-ietf-moq-transport-17
   * https://www.ietf.org/archive/id/draft-ietf-moq-transport-17.txt
   */
  DRAFT_17: 4278190097,
  /**
   * draft-ietf-moq-transport-18
   * https://www.ietf.org/archive/id/draft-ietf-moq-transport-18.txt
   */
  DRAFT_18: 4278190098,
  /**
   * draft-ietf-moq-transport-19
   * https://www.ietf.org/archive/id/draft-ietf-moq-transport-19.txt
   */
  DRAFT_19: 4278190099,
  /**
   * draft-ietf-moq-transport-20
   * https://www.ietf.org/archive/id/draft-ietf-moq-transport-20.txt
   */
  DRAFT_20: 4278190100,
  /**
   * draft-ietf-moq-transport-21
   * https://www.ietf.org/archive/id/draft-ietf-moq-transport-21.txt
   */
  DRAFT_21: 4278190101
};
var ALPN = {
  DRAFT_14: "moq-00",
  DRAFT_15: "moqt-15",
  DRAFT_16: "moqt-16",
  DRAFT_17: "moqt-17",
  DRAFT_18: "moqt-18",
  DRAFT_19: "moqt-19",
  DRAFT_20: "moqt-20",
  DRAFT_21: "moqt-21"
};
var VERSION_NAMES = {
  [Version.DRAFT_07]: "moq-transport-07",
  [Version.DRAFT_14]: "moq-transport-14",
  [Version.DRAFT_15]: "moq-transport-15",
  [Version.DRAFT_16]: "moq-transport-16",
  [Version.DRAFT_17]: "moq-transport-17",
  [Version.DRAFT_18]: "moq-transport-18",
  [Version.DRAFT_19]: "moq-transport-19",
  [Version.DRAFT_20]: "moq-transport-20",
  [Version.DRAFT_21]: "moq-transport-21"
};
function versionName(v) {
  return VERSION_NAMES[v] ?? `unknown(0x${v.toString(16)})`;
}

// vendor/moq/js/net/src/util/timeout.ts
var TimeoutError = class extends Error {
  constructor(message) {
    super(message);
    this.name = "TimeoutError";
  }
};
function withTimeout(promise, ms, message) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new TimeoutError(message)), ms);
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

// vendor/moq/js/net/src/util/utf8.ts
var decoder = new TextDecoder("utf-8", { fatal: true });
function decodeUtf8(buffer) {
  return decoder.decode(buffer);
}

// vendor/moq/js/net/src/varint.ts
var varint_exports = {};
__export(varint_exports, {
  MAX_U14: () => MAX_U14,
  MAX_U30: () => MAX_U30,
  MAX_U53: () => MAX_U53,
  MAX_U6: () => MAX_U6,
  decode: () => decode2,
  decodeBigInt: () => decodeBigInt,
  decodeLeadingOnes: () => decodeLeadingOnes,
  encode: () => encode2,
  encodeLeadingOnes: () => encodeLeadingOnes,
  encodeLeadingOnesTo: () => encodeLeadingOnesTo,
  encodeTo: () => encodeTo,
  size: () => size,
  sizeLeadingOnes: () => sizeLeadingOnes
});
var MAX_U6 = 2 ** 6 - 1;
var MAX_U14 = 2 ** 14 - 1;
var MAX_U30 = 2 ** 30 - 1;
var MAX_U53 = Number.MAX_SAFE_INTEGER;
var MAX_U64 = (1n << 64n) - 1n;
function sizeLeadingOnes(v) {
  const b = BigInt(v);
  if (b < 0n) throw new RangeError(`value is negative: ${v}`);
  if (b > MAX_U64) throw new RangeError(`value exceeds 64 bits: ${v}`);
  if (b < 1n << 7n) return 1;
  if (b < 1n << 14n) return 2;
  if (b < 1n << 21n) return 3;
  if (b < 1n << 28n) return 4;
  if (b < 1n << 35n) return 5;
  if (b < 1n << 42n) return 6;
  if (b < 1n << 56n) return 8;
  return 9;
}
function encodeLeadingOnesTo(dst, v) {
  const x = BigInt(v);
  if (x < 0n) throw new RangeError(`underflow, value is negative: ${v}`);
  if (x > MAX_U64) throw new RangeError(`value exceeds 64 bits: ${v}`);
  const view = new DataView(dst);
  if (x < 1n << 7n) {
    view.setUint8(0, Number(x));
    return new Uint8Array(dst, 0, 1);
  }
  if (x < 1n << 14n) {
    view.setUint8(0, 128 | Number(x >> 8n));
    view.setUint8(1, Number(x & 0xffn));
    return new Uint8Array(dst, 0, 2);
  }
  if (x < 1n << 21n) {
    view.setUint8(0, 192 | Number(x >> 16n));
    view.setUint16(1, Number(x & 0xffffn));
    return new Uint8Array(dst, 0, 3);
  }
  if (x < 1n << 28n) {
    view.setUint8(0, 224 | Number(x >> 24n));
    view.setUint8(1, Number(x >> 16n & 0xffn));
    view.setUint16(2, Number(x & 0xffffn));
    return new Uint8Array(dst, 0, 4);
  }
  if (x < 1n << 35n) {
    view.setUint8(0, 240 | Number(x >> 32n));
    view.setUint32(1, Number(x & 0xffffffffn));
    return new Uint8Array(dst, 0, 5);
  }
  if (x < 1n << 42n) {
    view.setUint8(0, 248 | Number(x >> 40n));
    view.setUint8(1, Number(x >> 32n & 0xffn));
    view.setUint32(2, Number(x & 0xffffffffn));
    return new Uint8Array(dst, 0, 6);
  }
  if (x < 1n << 56n) {
    view.setUint8(0, 254);
    view.setUint8(1, Number(x >> 48n & 0xffn));
    view.setUint16(2, Number(x >> 32n & 0xffffn));
    view.setUint32(4, Number(x & 0xffffffffn));
    return new Uint8Array(dst, 0, 8);
  }
  view.setUint8(0, 255);
  view.setBigUint64(1, x);
  return new Uint8Array(dst, 0, 9);
}
function encodeLeadingOnes(v) {
  return encodeLeadingOnesTo(new ArrayBuffer(9), v);
}
function decodeLeadingOnes(buf) {
  if (buf.length === 0) throw new Error("buffer is empty");
  const b = buf[0];
  let ones = 0;
  for (let bit = 7; bit >= 0; bit--) {
    if (b & 1 << bit) ones++;
    else break;
  }
  let totalSize;
  if (ones <= 5) totalSize = ones + 1;
  else if (ones === 6) totalSize = 7;
  else if (ones === 7) totalSize = 8;
  else totalSize = 9;
  if (buf.length < totalSize) {
    throw new Error(`buffer too short: need ${totalSize} bytes, have ${buf.length}`);
  }
  const view = new DataView(buf.buffer, buf.byteOffset, totalSize);
  const remain = buf.subarray(totalSize);
  let value;
  switch (ones) {
    case 0:
      value = BigInt(b);
      break;
    case 1:
      value = BigInt(b & 63) << 8n | BigInt(buf[1]);
      break;
    case 2:
      value = BigInt(b & 31) << 16n | BigInt(view.getUint16(1));
      break;
    case 3:
      value = BigInt(b & 15) << 24n | BigInt(buf[1]) << 16n | BigInt(buf[2]) << 8n | BigInt(buf[3]);
      break;
    case 4:
      value = BigInt(b & 7) << 32n | BigInt(view.getUint32(1));
      break;
    case 5:
      value = BigInt(b & 3) << 40n | BigInt(buf[1]) << 32n | BigInt(buf[2]) << 24n | BigInt(buf[3]) << 16n | BigInt(buf[4]) << 8n | BigInt(buf[5]);
      break;
    case 6: {
      value = BigInt(b & 1) << 48n | BigInt(buf[1]) << 40n | BigInt(buf[2]) << 32n | BigInt(buf[3]) << 24n | BigInt(buf[4]) << 16n | BigInt(buf[5]) << 8n | BigInt(buf[6]);
      break;
    }
    case 7: {
      const hi = new Uint8Array(8);
      hi[0] = 0;
      hi.set(buf.subarray(1, 8), 1);
      value = new DataView(hi.buffer).getBigUint64(0);
      break;
    }
    case 8: {
      value = new DataView(buf.buffer, buf.byteOffset + 1, 8).getBigUint64(0);
      break;
    }
    default:
      throw new Error("impossible");
  }
  return [value, remain];
}
function size(v) {
  if (v <= MAX_U6) return 1;
  if (v <= MAX_U14) return 2;
  if (v <= MAX_U30) return 4;
  if (v <= MAX_U53) return 8;
  throw new Error(`overflow, value larger than 53-bits: ${v}`);
}
function setUint8(dst, v) {
  const buffer = new Uint8Array(dst, 0, 1);
  buffer[0] = v;
  return buffer;
}
function setUint16(dst, v) {
  const view = new DataView(dst, 0, 2);
  view.setUint16(0, v);
  return new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
}
function setUint32(dst, v) {
  const view = new DataView(dst, 0, 4);
  view.setUint32(0, v);
  return new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
}
function setUint64(dst, v) {
  const view = new DataView(dst, 0, 8);
  view.setBigUint64(0, v);
  return new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
}
var MAX_U62 = 2n ** 62n - 1n;
function encodeTo(dst, v) {
  const b = BigInt(v);
  if (b < 0n) {
    throw new Error(`underflow, value is negative: ${v}`);
  }
  if (b > MAX_U62) {
    throw new Error(`overflow, value larger than 62-bits: ${v}`);
  }
  const n = Number(b);
  if (n <= MAX_U6) {
    return setUint8(dst, n);
  }
  if (n <= MAX_U14) {
    return setUint16(dst, n | 16384);
  }
  if (n <= MAX_U30) {
    return setUint32(dst, n | 2147483648);
  }
  return setUint64(dst, b | 0xc000000000000000n);
}
function encode2(v) {
  return encodeTo(new ArrayBuffer(8), v);
}
function decodeBigInt(buf) {
  if (buf.length === 0) {
    throw new Error("buffer is empty");
  }
  const size2 = 1 << ((buf[0] & 192) >> 6);
  if (buf.length < size2) {
    throw new Error(`buffer too short: need ${size2} bytes, have ${buf.length}`);
  }
  const view = new DataView(buf.buffer, buf.byteOffset, size2);
  const remain = buf.subarray(size2);
  let value;
  if (size2 === 1) {
    value = BigInt(buf[0] & 63);
  } else if (size2 === 2) {
    value = BigInt(view.getUint16(0) & 16383);
  } else if (size2 === 4) {
    value = BigInt(view.getUint32(0) & 1073741823);
  } else if (size2 === 8) {
    value = view.getBigUint64(0) & 0x3fffffffffffffffn;
  } else {
    throw new Error("impossible");
  }
  return [value, remain];
}
function decode2(buf) {
  const [value, remain] = decodeBigInt(buf);
  return [Number(value), remain];
}

// vendor/moq/js/net/src/stream.ts
var MAX_U31 = 2 ** 31 - 1;
var MAX_READ_SIZE = 1024 * 1024 * 64;
function sendOptions(options) {
  return { sendOrder: options?.sendOrder, waitUntilAvailable: options?.waitUntilAvailable ?? true };
}
var OPEN_TIMEOUT_MS = 1e4;
async function openWithin(opening, timeout, discard) {
  try {
    return await withTimeout(opening, timeout, `stream open timed out after ${timeout}ms waiting for a slot`);
  } catch (err) {
    opening.then(discard).catch(() => void 0);
    throw err;
  }
}
function isLeadingOnes(version2) {
  return version2 !== void 0 && version2 !== Version.DRAFT_14 && version2 !== Version.DRAFT_15 && version2 !== Version.DRAFT_16;
}
var Stream = class _Stream {
  reader;
  writer;
  constructor(props) {
    const writer = props.writer ?? (props.writable && new Writer(props.writable, props.version));
    const reader = props.reader ?? (props.readable && new Reader(props.readable, void 0, props.version));
    if (!writer || !reader) throw new Error("stream needs both halves");
    this.writer = writer;
    this.reader = reader;
  }
  static async accept(quic, version2) {
    for (; ; ) {
      const reader = quic.incomingBidirectionalStreams.getReader();
      const next = await reader.read();
      reader.releaseLock();
      if (next.done) return;
      const { readable, writable } = next.value;
      return new _Stream({ readable, writable, version: version2 });
    }
  }
  /**
   * Open an outgoing bidirectional stream.
   * @param quic - The session to open it on
   * @param options - The version its varints encode with, and the send order ranking it
   *   against the session's other streams
   */
  static async open(quic, options) {
    const { readable, writable } = await openWithin(
      quic.createBidirectionalStream(sendOptions(options)),
      options?.timeout ?? OPEN_TIMEOUT_MS,
      (stream) => {
        void stream.writable.abort().catch(() => void 0);
        void stream.readable.cancel().catch(() => void 0);
      }
    );
    return new _Stream({ readable, writable, version: options?.version });
  }
  close() {
    this.writer.close();
    this.reader.stop(new Error("cancel"));
  }
  abort(reason2) {
    this.writer.reset(reason2);
    this.reader.stop(reason2);
  }
};
var Reader = class {
  #buffer;
  #stream;
  // if undefined, the buffer is consumed then EOF
  #reader;
  #closed;
  version;
  constructor(stream, buffer, version2) {
    this.#buffer = buffer ?? new Uint8Array();
    this.#stream = stream;
    this.#reader = this.#stream?.getReader();
    this.version = version2;
  }
  // Adds more data to the buffer, returning true if more data was added.
  async #fill() {
    if (!this.#reader) {
      return false;
    }
    const result = await this.#reader.read().catch((err) => {
      throw fromTransport(err);
    });
    if (result.done) {
      return false;
    }
    if (result.value.byteLength === 0) {
      throw new Error("unexpected empty chunk");
    }
    const buffer = result.value;
    if (this.#buffer.byteLength === 0) {
      this.#buffer = new Uint8Array(buffer);
    } else {
      const temp = new Uint8Array(this.#buffer.byteLength + buffer.byteLength);
      temp.set(this.#buffer);
      temp.set(buffer, this.#buffer.byteLength);
      this.#buffer = temp;
    }
    return true;
  }
  // Add more data to the buffer until it's at least size bytes.
  async #fillTo(size2) {
    if (size2 > MAX_READ_SIZE) {
      throw new Error(`read size ${size2} exceeds max size ${MAX_READ_SIZE}`);
    }
    while (this.#buffer.byteLength < size2) {
      if (!await this.#fill()) {
        throw new Error("unexpected end of stream");
      }
    }
  }
  // Consumes the first size bytes of the buffer.
  #slice(size2) {
    const result = new Uint8Array(this.#buffer.buffer, this.#buffer.byteOffset, size2);
    this.#buffer = new Uint8Array(
      this.#buffer.buffer,
      this.#buffer.byteOffset + size2,
      this.#buffer.byteLength - size2
    );
    return result;
  }
  async read(size2) {
    if (size2 === 0) return new Uint8Array();
    await this.#fillTo(size2);
    return this.#slice(size2);
  }
  async readAll() {
    while (await this.#fill()) {
    }
    return this.#slice(this.#buffer.byteLength);
  }
  async string() {
    const length = await this.u53();
    const buffer = await this.read(length);
    return decodeUtf8(buffer);
  }
  async bool() {
    const v = await this.u8();
    if (v === 0) return false;
    if (v === 1) return true;
    throw new Error("invalid bool value");
  }
  async u8() {
    await this.#fillTo(1);
    return this.#slice(1)[0];
  }
  async u16() {
    await this.#fillTo(2);
    const view = new DataView(this.#buffer.buffer, this.#buffer.byteOffset, 2);
    const result = view.getUint16(0);
    this.#slice(2);
    return result;
  }
  // Returns a Number using 53-bits, the max Javascript can use for integer math.
  async u53() {
    const v = await this.u62();
    if (v > MAX_U53) {
      throw new Error(`value larger than 53-bits: ${v.toString()}`);
    }
    return Number(v);
  }
  // NOTE: Returns a bigint instead of a number since it may be larger than 53-bits
  async u62() {
    if (isLeadingOnes(this.version)) {
      return this.#readLeadingOnes();
    }
    return this.#readQuicVarint();
  }
  async #readQuicVarint() {
    await this.#fillTo(1);
    const size2 = (this.#buffer[0] & 192) >> 6;
    if (size2 === 0) {
      const first = this.#slice(1)[0];
      return BigInt(first) & 0x3fn;
    }
    if (size2 === 1) {
      await this.#fillTo(2);
      const slice2 = this.#slice(2);
      const view2 = new DataView(slice2.buffer, slice2.byteOffset, slice2.byteLength);
      return BigInt(view2.getUint16(0)) & 0x3fffn;
    }
    if (size2 === 2) {
      await this.#fillTo(4);
      const slice2 = this.#slice(4);
      const view2 = new DataView(slice2.buffer, slice2.byteOffset, slice2.byteLength);
      return BigInt(view2.getUint32(0)) & 0x3fffffffn;
    }
    await this.#fillTo(8);
    const slice = this.#slice(8);
    const view = new DataView(slice.buffer, slice.byteOffset, slice.byteLength);
    return view.getBigUint64(0) & 0x3fffffffffffffffn;
  }
  async #readLeadingOnes() {
    await this.#fillTo(1);
    const b = this.#buffer[0];
    let ones = 0;
    for (let bit = 7; bit >= 0; bit--) {
      if (b & 1 << bit) ones++;
      else break;
    }
    if (ones === 6 && this.version === Version.DRAFT_17) {
      throw new Error("invalid leading-ones varint: 1111110x prefix is reserved on draft-17");
    }
    let totalSize;
    if (ones <= 5) totalSize = ones + 1;
    else if (ones === 6) totalSize = 7;
    else if (ones === 7) totalSize = 8;
    else totalSize = 9;
    await this.#fillTo(totalSize);
    const slice = this.#slice(totalSize);
    const [value] = decodeLeadingOnes(slice);
    return value;
  }
  // Returns false if there is more data to read, blocking if it hasn't been received yet.
  async done() {
    if (this.#buffer.byteLength > 0) return false;
    return !await this.#fill();
  }
  stop(reason2) {
    this.#reader?.cancel(reason2).catch(() => void 0);
  }
  // Decoded like #fill: a caller racing this against a read must not get a different error
  // shape depending on which one won. Derived once, so racing it per frame doesn't allocate.
  get closed() {
    this.#closed ??= (this.#reader?.closed ?? Promise.resolve()).catch((err) => {
      throw fromTransport(err);
    });
    return this.#closed;
  }
};
var COALESCE_HIGH_WATER = 64 * 1024;
var scheduleFlush = (() => {
  if (typeof MessageChannel === "function") {
    const channel = new MessageChannel();
    const queue = [];
    channel.port1.onmessage = () => {
      const batch = queue.splice(0);
      for (const flush of batch) flush();
    };
    channel.port1.unref?.();
    channel.port2.unref?.();
    return (flush) => {
      queue.push(flush);
      if (queue.length === 1) channel.port2.postMessage(0);
    };
  }
  return (flush) => void setTimeout(flush, 0);
})();
var Writer = class _Writer {
  #writer;
  #stream;
  #closed;
  #pending = [];
  #pendingBytes = 0;
  #scheduled = false;
  #inflight = Promise.resolve();
  #failure;
  #ending = false;
  // Scratch buffer for writing varints.
  // Fixed at 9 bytes (leading-ones max).
  #scratch;
  version;
  constructor(stream, version2) {
    this.#stream = stream;
    this.#scratch = new ArrayBuffer(9);
    this.#writer = this.#stream.getWriter();
    this.version = version2;
  }
  /**
   * Rank this stream against the session's others, where HIGHER values are sent first.
   *
   * A send order only schedules the local end, so a stream the peer opened has to be ranked
   * here rather than at the peer's {@link open}.
   *
   * The spec makes `sendOrder` a settable attribute on every {@link SendStream}. Where the
   * interface isn't implemented (Chrome as of writing, a mock, a polyfill) this just sets an
   * ignored property, the same way an ignored `sendOrder` option does at {@link open}.
   */
  setPriority(sendOrder2) {
    this.#stream.sendOrder = sendOrder2;
  }
  async bool(v) {
    await this.write(setUint82(this.#scratch, v ? 1 : 0));
  }
  async u8(v) {
    await this.write(setUint82(this.#scratch, v));
  }
  async u16(v) {
    await this.write(setUint162(this.#scratch, v));
  }
  async i32(v) {
    if (Math.abs(v) > MAX_U31) {
      throw new Error(`overflow, value larger than 32-bits: ${v.toString()}`);
    }
    await this.write(setInt32(this.#scratch, v));
  }
  async u53(v) {
    if (v > MAX_U53) {
      console.warn(`value larger than 53-bits; use u62 instead (precision lost): ${v.toString()}`);
    }
    if (isLeadingOnes(this.version)) {
      await this.write(encodeLeadingOnesTo(this.#scratch, v));
    } else {
      await this.write(encodeTo(this.#scratch, v));
    }
  }
  async u62(v) {
    if (isLeadingOnes(this.version)) {
      await this.write(encodeLeadingOnesTo(this.#scratch, v));
    } else {
      await this.write(encodeTo(this.#scratch, v));
    }
  }
  async write(v) {
    if (this.#failure !== void 0) throw fromTransport(this.#failure);
    if (this.#ending) throw new Error("writer is closed");
    if (v.byteLength === 0) return;
    this.#pending.push(v.slice());
    this.#pendingBytes += v.byteLength;
    if (!this.#scheduled) {
      this.#scheduled = true;
      scheduleFlush(() => this.#flush());
    }
    if (this.#pendingBytes >= COALESCE_HIGH_WATER) {
      this.#flush();
      await this.#inflight;
      if (this.#failure !== void 0) throw fromTransport(this.#failure);
    }
  }
  #flush() {
    this.#scheduled = false;
    if (this.#pending.length === 0 || this.#failure !== void 0) return;
    let chunk;
    if (this.#pending.length === 1) {
      chunk = this.#pending[0];
    } else {
      chunk = new Uint8Array(this.#pendingBytes);
      let offset = 0;
      for (const part of this.#pending) {
        chunk.set(part, offset);
        offset += part.byteLength;
      }
    }
    this.#pending = [];
    this.#pendingBytes = 0;
    this.#inflight = this.#writer.write(chunk).catch((err) => {
      this.#failure ??= err;
    });
  }
  async string(str) {
    const data = new TextEncoder().encode(str);
    await this.u53(data.byteLength);
    await this.write(data);
  }
  close() {
    this.#flush();
    this.#ending = true;
    this.#writer.close().catch(() => void 0);
  }
  // Mirrors Reader.closed: a STOP_SENDING reaches a caller racing this with the same
  // typed code it would get from a write.
  get closed() {
    this.#closed ??= this.#writer.closed.catch((err) => {
      throw fromTransport(err);
    });
    return this.#closed;
  }
  reset(reason2) {
    this.#pending = [];
    this.#pendingBytes = 0;
    this.#ending = true;
    this.#writer.abort(reason2).catch(() => void 0);
  }
  /**
   * Open an outgoing unidirectional stream.
   * @param quic - The session to open it on
   * @param options - The version its varints encode with, and the send order ranking it
   *   against the session's other streams
   */
  static async open(quic, options) {
    const writable = await openWithin(
      quic.createUnidirectionalStream(sendOptions(options)),
      options?.timeout ?? OPEN_TIMEOUT_MS,
      (stream) => void stream.abort().catch(() => void 0)
    );
    return new _Writer(writable, options?.version);
  }
  /**
   * Like {@link Writer.open}, but gives up when `cancel` settles or `timeout` elapses,
   * returning undefined so the caller can drop whatever it meant to send. A stream that
   * opens after that is reset rather than leaked. A real transport failure still throws.
   *
   * Worth using even with `waitUntilAvailable: false`, since an implementation may park
   * an over-limit open instead of rejecting it.
   */
  static async tryOpen(quic, options) {
    const cancelled = options.cancel.then(
      () => void 0,
      () => void 0
    );
    const open = _Writer.open(quic, options);
    try {
      const stream = await Promise.race([cancelled, open]);
      if (stream) return stream;
    } catch (err) {
      if (!(err instanceof TimeoutError)) throw err;
      return void 0;
    }
    const abandoned = new Error("abandoned waiting for a stream slot");
    open.then((w) => w.reset(abandoned)).catch(() => void 0);
    return void 0;
  }
};
function setUint82(dst, v) {
  const buffer = new Uint8Array(dst, 0, 1);
  buffer[0] = v;
  return buffer;
}
function setUint162(dst, v) {
  const view = new DataView(dst, 0, 2);
  view.setUint16(0, v);
  return new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
}
function setInt32(dst, v) {
  const view = new DataView(dst, 0, 4);
  view.setInt32(0, v);
  return new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
}
var Readers = class {
  #reader;
  #version;
  constructor(quic, version2) {
    this.#reader = quic.incomingUnidirectionalStreams.getReader();
    this.#version = version2;
  }
  async next() {
    const next = await this.#reader.read();
    if (next.done) return;
    return new Reader(next.value, void 0, this.#version);
  }
  close() {
    this.#reader.cancel();
  }
};

// vendor/moq/js/net/src/ietf/namespace.ts
function toTuple(namespace) {
  if (namespace === "") return [];
  const parts2 = [""];
  for (let i = 0; i < namespace.length; i++) {
    const ch = namespace[i];
    if (ch === "/") {
      parts2.push("");
    } else if (ch === "\\" && (namespace[i + 1] === "/" || namespace[i + 1] === "\\")) {
      parts2[parts2.length - 1] += namespace[++i];
    } else {
      parts2[parts2.length - 1] += ch;
    }
  }
  return parts2;
}
function fromTuple(parts2) {
  return parts2.map((part) => part.replaceAll("\\", "\\\\").replaceAll("/", "\\/")).join("/");
}
async function encode3(w, namespace) {
  const parts2 = toTuple(namespace);
  if (parts2.length > MAX_PARTS) {
    throw new Error(`namespace exceeds ${MAX_PARTS} parts`);
  }
  await w.u53(parts2.length);
  for (const part of parts2) {
    await w.string(part);
  }
}
async function decode3(r) {
  const count = await r.u53();
  if (count > MAX_PARTS) {
    throw new Error(`namespace exceeds ${MAX_PARTS} parts`);
  }
  const parts2 = [];
  for (let i = 0; i < count; i++) {
    parts2.push(await r.string());
  }
  return fromTuple(parts2);
}

// vendor/moq/js/net/src/ietf/adapter.ts
var NativeSession = class {
  #quic;
  // moq-transport reserves even request IDs for the client and odd for the server,
  // so the two peers' ID spaces never overlap.
  #requestId;
  version;
  constructor(quic, version2, client) {
    this.#quic = quic;
    this.version = version2;
    this.#requestId = client ? 0n : 1n;
  }
  async openBi() {
    return Stream.open(this.#quic, { version: this.version });
  }
  async acceptBi() {
    return Stream.accept(this.#quic, this.version);
  }
  async nextRequestId() {
    const id = this.#requestId;
    this.#requestId += 2n;
    return id;
  }
  /** Closes the underlying WebTransport session. */
  close() {
    this.#quic.close();
  }
};
var Route = {
  NewRequest: 0,
  // Create virtual bidi stream, push initial message
  Response: 1,
  // Push message to existing stream (keep open)
  ErrorResponse: 2,
  // Push message to existing stream, then close
  CloseStream: 3,
  // Close stream recv (no bytes pushed)
  FollowUp: 4,
  // Push follow-up message to existing stream
  MaxRequestId: 5,
  // Update flow control
  Ignore: 6,
  // Connection-level, no routing
  GoAway: 7
  // Terminal
};
var ControlStreamAdapter = class {
  // WebTransport session (for opening real bidi streams in v16)
  #quic;
  // Control stream
  #reader;
  #writer;
  #writeMutex = new Mutex();
  version;
  // Virtual streams keyed by requestId
  #streams = /* @__PURE__ */ new Map();
  // Namespace → requestId reverse lookups for v14/v15 namespace-keyed messages.
  // DONE names an incoming announcement and CANCEL names an outgoing one, so a relay
  // may legitimately hold the same namespace in both maps at once.
  #incomingNamespaces = /* @__PURE__ */ new Map();
  #outgoingNamespaces = /* @__PURE__ */ new Map();
  // requestId → namespace reverse lookup, one entry per request including a refused
  // duplicate, so #forget can clean up whichever request goes away.
  #namespacesByRequestId = /* @__PURE__ */ new Map();
  // SubscribeNamespace requestIds — for routing 0x08/0x0E entries that lack requestId (v14/v15)
  #subscribeNamespaces = /* @__PURE__ */ new Set();
  // Incoming stream queue (for acceptBi)
  #incomingQueue = [];
  #incomingWaiters = [];
  // Request ID flow control. moq-transport reserves even request IDs for the
  // client and odd for the server, so the two peers' ID spaces never overlap.
  // This matters here because a single `#streams` map routes every request by
  // ID: overlapping spaces would let an inbound request clobber the routing
  // entry of an outbound one with the same number (e.g. an inbound
  // PUBLISH_NAMESPACE stealing a pending SUBSCRIBE's slot, so SUBSCRIBE_OK is
  // delivered to the wrong virtual stream).
  #requestId;
  #maxRequestId;
  #maxRequestIdResolves = [];
  #closed = false;
  constructor(quic, controlStream, version2, maxRequestId, client) {
    this.#quic = quic;
    this.#reader = controlStream.reader;
    this.#reader.version = version2;
    this.#writer = controlStream.writer;
    this.#writer.version = version2;
    this.version = version2;
    this.#maxRequestId = maxRequestId;
    this.#requestId = client ? 0n : 1n;
  }
  /**
   * Accept the next incoming virtual bidi stream.
   * Blocks until a new request arrives on the control stream.
   */
  async acceptBi() {
    if (this.#closed) return void 0;
    const queued = this.#incomingQueue.shift();
    if (queued) return queued;
    return new Promise((resolve2) => {
      this.#incomingWaiters.push(resolve2);
    });
  }
  /**
   * Open an outgoing virtual bidi stream.
   * Buffers writes until the first full message is available, parses the
   * requestId (and namespace for PublishNamespace), self-registers, then
   * flushes. Subsequent writes go directly to the control stream.
   */
  openBi() {
    let controller;
    let registeredRequestId;
    const readable = new ReadableStream({
      start(c) {
        controller = c;
      },
      cancel: () => {
        if (registeredRequestId !== void 0) {
          this.#forget(registeredRequestId);
        }
      }
    });
    let buffer = new Uint8Array(0);
    let registered = false;
    const sendWritable = new WritableStream({
      write: async (chunk) => {
        const newBuf = new Uint8Array(buffer.length + chunk.length);
        newBuf.set(buffer);
        newBuf.set(chunk, buffer.length);
        buffer = newBuf;
        for (; ; ) {
          const boundary = this.#messageSize(buffer);
          if (boundary === void 0) break;
          const toFlush = buffer.subarray(0, boundary);
          buffer = buffer.subarray(boundary);
          if (!registered) {
            const parsed = await this.#tryParseOutgoing(toFlush);
            if (parsed) {
              registeredRequestId = parsed.requestId;
              this.#streams.set(parsed.requestId, { controller });
              registered = true;
            }
          }
          await this.#writeMutex.runExclusive(() => this.#writer.write(toFlush));
        }
      }
    });
    const stream = new Stream({ readable, writable: sendWritable });
    stream.reader.version = this.version;
    stream.writer.version = this.version;
    return stream;
  }
  /**
   * Open a real WebTransport bidi stream (for v16 SubscribeNamespace).
   */
  async openNativeBi() {
    return Stream.open(this.#quic, { version: this.version });
  }
  /**
   * Allocate the next request ID, blocking if flow control limit reached.
   */
  async nextRequestId() {
    for (; ; ) {
      if (this.#closed) return void 0;
      const id = this.#requestId;
      if (id < this.#maxRequestId) {
        this.#requestId += 2n;
        return id;
      }
      await new Promise((resolve2) => {
        this.#maxRequestIdResolves.push(resolve2);
      });
    }
  }
  /**
   * Main run loop — reads control stream messages and routes to virtual streams.
   * Must be called after construction. Runs until the control stream closes.
   */
  async run() {
    try {
      if (this.version === Version.DRAFT_16) {
        void this.#acceptNativeBidis();
      }
      for (; ; ) {
        const done = await this.#reader.done();
        if (done) break;
        const typeId = await this.#reader.u53();
        const size2 = await this.#reader.u16();
        const body = await this.#reader.read(size2);
        const classified = await this.#classify(typeId, body);
        if (classified.route === Route.GoAway) {
          console.warn("received GOAWAY on control stream");
          return;
        }
        const { route, requestId } = classified;
        switch (route) {
          case Route.NewRequest:
            this.#newRequest(typeId, size2, body, requestId);
            break;
          case Route.Response:
            this.#pushMessage(requestId, typeId, size2, body);
            break;
          case Route.ErrorResponse:
            this.#pushMessage(requestId, typeId, size2, body);
            this.#closeStream(requestId);
            break;
          case Route.CloseStream:
            this.#closeStream(requestId);
            break;
          case Route.FollowUp:
            this.#pushMessage(requestId, typeId, size2, body);
            break;
          case Route.MaxRequestId:
            this.#maxRequestId = requestId;
            for (const resolve2 of this.#maxRequestIdResolves) resolve2();
            this.#maxRequestIdResolves = [];
            break;
        }
      }
    } finally {
      this.close();
    }
  }
  /** Accept real WebTransport bidi streams and queue them for acceptBi (v16). */
  async #acceptNativeBidis() {
    try {
      for (; ; ) {
        const stream = await Stream.accept(this.#quic, this.version);
        if (!stream) break;
        const waiter = this.#incomingWaiters.shift();
        if (waiter) {
          waiter(stream);
        } else {
          this.#incomingQueue.push(stream);
        }
      }
    } catch {
    }
  }
  #newRequest(typeId, size2, body, requestId) {
    let controller;
    const readable = new ReadableStream({
      start(c) {
        controller = c;
      },
      cancel: () => {
        this.#forget(requestId);
      }
    });
    const sendWritable = this.#createSendWritable();
    const stream = new Stream({ readable, writable: sendWritable });
    stream.reader.version = this.version;
    stream.writer.version = this.version;
    this.#streams.set(requestId, { controller });
    controller.enqueue(this.#encodeRaw(typeId, size2, body));
    const waiter = this.#incomingWaiters.shift();
    if (waiter) {
      waiter(stream);
    } else {
      this.#incomingQueue.push(stream);
    }
  }
  #pushMessage(requestId, typeId, size2, body) {
    const entry = this.#streams.get(requestId);
    if (!entry) {
      console.warn(`adapter: no stream for requestId=${requestId} typeId=0x${typeId.toString(16)}`);
      return;
    }
    try {
      entry.controller.enqueue(this.#encodeRaw(typeId, size2, body));
    } catch {
    }
  }
  #closeStream(requestId) {
    const entry = this.#streams.get(requestId);
    this.#forget(requestId);
    if (!entry) return;
    console.debug(`adapter: closing stream requestId=${requestId}`);
    try {
      entry.controller.close();
    } catch {
    }
  }
  /**
   * Drop every routing entry a request owns, keeping the two namespace maps in step.
   *
   * Runs even when the virtual stream is already gone, so a request that was torn down
   * locally still gives its namespace back.
   */
  #forget(requestId) {
    this.#streams.delete(requestId);
    this.#subscribeNamespaces.delete(requestId);
    const registered = this.#namespacesByRequestId.get(requestId);
    if (registered === void 0) return;
    this.#namespacesByRequestId.delete(requestId);
    const namespaces = this.#namespaceRequests(registered.direction);
    if (namespaces.get(registered.namespace) === requestId) namespaces.delete(registered.namespace);
  }
  /**
   * Returns the total byte size of the first complete message in buffer,
   * or undefined if the buffer doesn't contain a complete message yet.
   * Message format: [typeId varint][size u16 BE][body of `size` bytes]
   */
  #messageSize(buffer) {
    if (buffer.length === 0) return void 0;
    const typeSize = 1 << ((buffer[0] & 192) >> 6);
    if (buffer.length < typeSize) return void 0;
    const [, afterType] = decode2(buffer);
    if (afterType.length < 2) return void 0;
    const size2 = afterType[0] << 8 | afterType[1];
    const totalSize = buffer.length - afterType.length + 2 + size2;
    if (buffer.length < totalSize) return void 0;
    return totalSize;
  }
  /**
   * Try to parse the first outgoing message from accumulated bytes.
   * Returns the requestId if enough data is available, undefined otherwise.
   */
  async #tryParseOutgoing(buffer) {
    if (buffer.length === 0) return void 0;
    const typeSize = 1 << ((buffer[0] & 192) >> 6);
    if (buffer.length < typeSize) return void 0;
    const [typeId, afterType] = decode2(buffer);
    if (afterType.length < 2) return void 0;
    const size2 = afterType[0] << 8 | afterType[1];
    const bodyStart = afterType.subarray(2);
    if (bodyStart.length < size2) return void 0;
    const body = bodyStart.subarray(0, size2);
    const [reqId] = decode2(body);
    const requestId = BigInt(reqId);
    if (typeId === 6) {
      try {
        await this.#registerNamespace(body, "outgoing");
      } catch {
      }
    }
    if (typeId === 17) {
      this.#subscribeNamespaces.add(requestId);
    }
    return { requestId };
  }
  /**
   * Decode a PublishNamespace body and register its namespace for the v14/v15
   * namespace-keyed withdrawals. Returns the request ID it carried.
   *
   * The one decode site for both directions: outgoing announcements go through
   * #tryParseOutgoing and incoming ones through #classify. The direction selects the
   * map that the peer's DONE or CANCEL will later consult.
   */
  async #registerNamespace(body, direction) {
    const r = new Reader(void 0, body, this.version);
    const requestId = await r.u62();
    const namespace = await decode3(r);
    const namespaces = this.#namespaceRequests(direction);
    this.#namespacesByRequestId.set(requestId, { namespace, direction });
    if (!namespaces.has(namespace)) namespaces.set(namespace, requestId);
    return requestId;
  }
  #namespaceRequests(direction) {
    return direction === "incoming" ? this.#incomingNamespaces : this.#outgoingNamespaces;
  }
  /** Create a WritableStream that buffers and writes complete messages to the control stream under mutex. */
  #createSendWritable() {
    let buffer = new Uint8Array(0);
    return new WritableStream({
      write: async (chunk) => {
        const newBuf = new Uint8Array(buffer.length + chunk.length);
        newBuf.set(buffer);
        newBuf.set(chunk, buffer.length);
        buffer = newBuf;
        for (; ; ) {
          const boundary = this.#messageSize(buffer);
          if (boundary === void 0) break;
          const toFlush = buffer.subarray(0, boundary);
          buffer = buffer.subarray(boundary);
          await this.#writeMutex.runExclusive(() => this.#writer.write(toFlush));
        }
      }
    });
  }
  /** Encode raw message bytes: [typeId varint][size u16 BE][body] */
  #encodeRaw(typeId, size2, body) {
    const typeIdBytes = encodeTo(new ArrayBuffer(9), typeId);
    const result = new Uint8Array(typeIdBytes.byteLength + 2 + body.byteLength);
    result.set(typeIdBytes, 0);
    const sizeView = new DataView(result.buffer, typeIdBytes.byteLength, 2);
    sizeView.setUint16(0, size2);
    result.set(body, typeIdBytes.byteLength + 2);
    return result;
  }
  /**
   * Classify a control message and extract its requestId for routing.
   */
  async #classify(typeId, body) {
    const readRequestId = async () => {
      const r = new Reader(void 0, body, this.version);
      return await r.u62();
    };
    const readNamespaceRequestId = async (direction) => {
      const r = new Reader(void 0, body, this.version);
      const namespace = await decode3(r);
      const namespaces = this.#namespaceRequests(direction);
      const requestId = namespaces.get(namespace);
      if (requestId === void 0) console.warn(`adapter: no announcement for namespace: ${namespace}`);
      return requestId;
    };
    switch (typeId) {
      // === FollowUp: route to existing stream ===
      case 2: {
        const requestId = await readRequestId();
        return { route: Route.FollowUp, requestId };
      }
      // === NewRequest: create virtual stream ===
      case 3:
      // Subscribe
      case 22:
      // Fetch
      case 29:
      // Publish
      case 13: {
        const requestId = await readRequestId();
        return { route: Route.NewRequest, requestId };
      }
      case 6: {
        const requestId = await this.#registerNamespace(body, "incoming");
        return { route: Route.NewRequest, requestId };
      }
      case 17: {
        if (this.version !== Version.DRAFT_14 && this.version !== Version.DRAFT_15) {
          throw new Error("unexpected SubscribeNamespace on control stream");
        }
        const requestId = await readRequestId();
        return { route: Route.NewRequest, requestId };
      }
      // === Response: push bytes, keep stream open ===
      case 4: {
        const requestId = await readRequestId();
        return { route: Route.Response, requestId };
      }
      case 24: {
        const requestId = await readRequestId();
        return { route: Route.Response, requestId };
      }
      case 30: {
        const requestId = await readRequestId();
        return { route: Route.Response, requestId };
      }
      case 7: {
        const requestId = await readRequestId();
        return { route: Route.Response, requestId };
      }
      case 18: {
        if (this.version !== Version.DRAFT_14) throw new Error("unexpected SubscribeNamespaceOk");
        const requestId = await readRequestId();
        return { route: Route.Response, requestId };
      }
      // === ErrorResponse: push bytes + close ===
      case 5: {
        const requestId = await readRequestId();
        return { route: Route.ErrorResponse, requestId };
      }
      case 25: {
        if (this.version !== Version.DRAFT_14) throw new Error("unexpected FetchError");
        const requestId = await readRequestId();
        return { route: Route.ErrorResponse, requestId };
      }
      case 31: {
        if (this.version !== Version.DRAFT_14) throw new Error("unexpected PublishError");
        const requestId = await readRequestId();
        return { route: Route.ErrorResponse, requestId };
      }
      case 8: {
        if (this.version === Version.DRAFT_14) {
          const requestId = await readRequestId();
          return { route: Route.ErrorResponse, requestId };
        }
        const subNs08 = this.#subscribeNamespaces.values().next().value;
        if (subNs08 === void 0) throw new Error("unexpected message 0x08: no SubscribeNamespace stream");
        return { route: Route.FollowUp, requestId: subNs08 };
      }
      case 14: {
        const subNs0e = this.#subscribeNamespaces.values().next().value;
        if (subNs0e === void 0) throw new Error("unexpected message 0x0e: no SubscribeNamespace stream");
        return { route: Route.FollowUp, requestId: subNs0e };
      }
      case 19: {
        if (this.version !== Version.DRAFT_14) throw new Error("unexpected SubscribeNamespaceError");
        const requestId = await readRequestId();
        return { route: Route.ErrorResponse, requestId };
      }
      // === CloseStream: close recv (no bytes pushed) ===
      case 10: {
        const requestId = await readRequestId();
        return { route: Route.CloseStream, requestId };
      }
      case 11: {
        const requestId = await readRequestId();
        return { route: Route.CloseStream, requestId };
      }
      case 23: {
        const requestId = await readRequestId();
        return { route: Route.CloseStream, requestId };
      }
      case 9: {
        if (this.version === Version.DRAFT_16) {
          const requestId2 = await readRequestId();
          return { route: Route.CloseStream, requestId: requestId2 };
        }
        const requestId = await readNamespaceRequestId("incoming");
        if (requestId === void 0) return { route: Route.Ignore, requestId: 0n };
        return { route: Route.CloseStream, requestId };
      }
      case 12: {
        if (this.version === Version.DRAFT_16) {
          const requestId2 = await readRequestId();
          return { route: Route.CloseStream, requestId: requestId2 };
        }
        const requestId = await readNamespaceRequestId("outgoing");
        if (requestId === void 0) return { route: Route.Ignore, requestId: 0n };
        return { route: Route.CloseStream, requestId };
      }
      case 20: {
        if (this.version !== Version.DRAFT_14 && this.version !== Version.DRAFT_15) {
          throw new Error("unexpected UnsubscribeNamespace");
        }
        const requestId = await readRequestId();
        return { route: Route.CloseStream, requestId };
      }
      // === Utility ===
      case 21: {
        const requestId = await readRequestId();
        return { route: Route.MaxRequestId, requestId };
      }
      case 26: {
        await readRequestId();
        return { route: Route.Ignore, requestId: 0n };
      }
      // === Terminal ===
      case 16:
        return { route: Route.GoAway };
      default:
        throw new Error(`unknown control message type: 0x${typeId.toString(16)}`);
    }
  }
  close() {
    if (this.#closed) return;
    this.#closed = true;
    console.debug("adapter: close() called");
    for (const entry of this.#streams.values()) {
      try {
        entry.controller.close();
      } catch {
      }
    }
    this.#streams.clear();
    for (const waiter of this.#incomingWaiters) {
      waiter(void 0);
    }
    this.#incomingWaiters = [];
    this.#incomingNamespaces.clear();
    this.#outgoingNamespaces.clear();
    this.#namespacesByRequestId.clear();
    this.#subscribeNamespaces.clear();
    for (const resolve2 of this.#maxRequestIdResolves) resolve2();
    this.#maxRequestIdResolves = [];
  }
};

// vendor/moq/js/net/src/ietf/cluster.ts
var cluster_exports = {};
__export(cluster_exports, {
  advertise: () => advertise,
  decodeParams: () => decodeParams,
  fromParams: () => fromParams,
  fromSetup: () => fromSetup,
  intoParams: () => intoParams,
  intoSetup: () => intoSetup,
  loops: () => loops,
  negotiated: () => negotiated,
  supported: () => supported
});

// scripts/moq-browser/node_modules/zod/v4/core/util.js
function jsonStringifyReplacer(_, value) {
  if (typeof value === "bigint")
    return value.toString();
  return value;
}
var captureStackTrace = "captureStackTrace" in Error ? Error.captureStackTrace : (..._args) => {
};
function clone(inst, def, params) {
  const cl = new inst._zod.constr(def ?? inst._zod.def);
  if (!def || params?.parent)
    cl._zod.parent = inst;
  return cl;
}
function normalizeParams(_params) {
  const params = _params;
  if (!params)
    return {};
  if (typeof params === "string")
    return { error: () => params };
  if (params?.message !== void 0) {
    if (params?.error !== void 0)
      throw new Error("Cannot specify both `message` and `error` params");
    params.error = params.message;
  }
  delete params.message;
  if (typeof params.error === "string")
    return { ...params, error: () => params.error };
  return params;
}
function aborted(x, startIndex = 0) {
  if (x.aborted === true)
    return true;
  for (let i = startIndex; i < x.issues.length; i++) {
    if (x.issues[i]?.continue !== true) {
      return true;
    }
  }
  return false;
}
function explicitlyAborted(x, startIndex = 0) {
  if (x.aborted === true)
    return true;
  for (let i = startIndex; i < x.issues.length; i++) {
    if (x.issues[i]?.continue === false) {
      return true;
    }
  }
  return false;
}
function unwrapMessage(message) {
  return typeof message === "string" ? message : message?.message;
}
function attachSchema(issues, start, inst) {
  var _a2;
  for (let i = start; i < issues.length; i++) {
    (_a2 = issues[i]).schema ?? (_a2.schema = inst);
  }
}
function finalizeIssue(iss, ctx, config2) {
  var _a2;
  const traits = iss.inst?._zod?.traits;
  if (traits?.has("$ZodType")) {
    if (traits.has("$ZodCheck"))
      (_a2 = iss).schema ?? (_a2.schema = iss.inst);
    else
      iss.schema = iss.inst;
  }
  const schemaError = iss.schema !== iss.inst ? iss.schema?._zod.def?.error : void 0;
  const message = iss.message ? iss.message : unwrapMessage(iss.inst?._zod.def?.error?.(iss)) ?? unwrapMessage(schemaError?.(iss)) ?? unwrapMessage(ctx?.error?.(iss)) ?? unwrapMessage(config2.customError?.(iss)) ?? unwrapMessage(config2.localeError?.(iss)) ?? "Invalid input";
  const full = {};
  for (const k of Object.keys(iss)) {
    if (k === "inst" || k === "schema" || k === "continue" || k === "input" || k === "__proto__")
      continue;
    full[k] = iss[k];
  }
  full.path ?? (full.path = []);
  full.message = message;
  if (ctx?.reportInput) {
    full.input = iss.input;
  }
  return full;
}
function issue(...args) {
  const [iss, input, inst] = args;
  if (typeof iss === "string") {
    return {
      message: iss,
      code: "custom",
      input,
      inst
    };
  }
  return { ...iss };
}
function members(proto, table) {
  for (const key in table) {
    const desc = Object.getOwnPropertyDescriptor(table, key);
    if (desc.get)
      Object.defineProperty(proto, key, { ...desc, enumerable: false });
    else
      defineBound(proto, key, desc.value);
  }
}
function own(inst, key, value, enumerable = true) {
  Object.defineProperty(inst, key, { configurable: true, writable: true, enumerable, value });
  return value;
}
function hide(inst, key, value) {
  return own(inst, key, value, false);
}
function defineBound(proto, key, fn) {
  Object.defineProperty(proto, key, {
    configurable: true,
    get() {
      return this == null ? fn : own(this, key, fn.bind(this));
    },
    set(value) {
      own(this, key, value);
    }
  });
}

// scripts/moq-browser/node_modules/zod/v4/core/core.js
var _a;
var _zodDesc = { value: void 0, enumerable: false };
var _E = "captureStackTrace" in Error ? Error : null;
function newError(Definition) {
  const E = _E;
  if (E) {
    const saved = E.stackTraceLimit;
    if (typeof saved === "number") {
      try {
        E.stackTraceLimit = 0;
      } catch {
        _E = null;
        return new Definition();
      }
      try {
        return new Definition();
      } finally {
        E.stackTraceLimit = saved;
      }
    }
  }
  return new Definition();
}
// @__NO_SIDE_EFFECTS__
function $constructor(name, initializer2, proto, params) {
  const zodProto = {};
  function Internals(def) {
    this.def = def;
    this.constr = _;
    this.traits = /* @__PURE__ */ new Set();
  }
  Internals.prototype = zodProto;
  const protoMembers = proto;
  const initialized = protoMembers && /* @__PURE__ */ new WeakSet();
  function init(inst, def) {
    if (!inst._zod) {
      _zodDesc.value = new Internals(def);
      try {
        Object.defineProperty(inst, "_zod", _zodDesc);
      } finally {
        _zodDesc.value = void 0;
      }
    } else if (inst._zod.traits.has(name)) {
      return;
    }
    inst._zod.traits.add(name);
    initializer2(inst, def);
    if (initialized) {
      const own2 = Object.getPrototypeOf(inst);
      const ctorProto = inst._zod.constr.prototype;
      let up = own2;
      while (up && up !== ctorProto)
        up = Object.getPrototypeOf(up);
      const target = up ?? own2;
      if (!initialized.has(target)) {
        initialized.add(target);
        members(target, protoMembers);
      }
    }
    const proto2 = _.prototype;
    for (const k in proto2) {
      if (!Object.prototype.hasOwnProperty.call(proto2, k))
        continue;
      if (!(k in inst)) {
        inst[k] = proto2[k].bind(inst);
      }
    }
  }
  const Parent = params?.Parent ?? Object;
  class Definition extends Parent {
  }
  Object.defineProperty(Definition, "name", { value: name });
  function _(def) {
    const inst = params?.Parent ? newError(Definition) : this;
    init(inst, def);
    const deferred = inst._zod.deferred;
    if (deferred) {
      for (const fn of deferred) {
        fn();
      }
      inst._zod.deferred = void 0;
    }
    const pp = globalThis.__zod_globalConfig?.postProcessor;
    if (pp)
      pp(inst);
    return inst;
  }
  Object.defineProperty(_, "init", { value: init });
  Object.defineProperty(_, Symbol.hasInstance, {
    value: (inst) => {
      if (params?.Parent && inst instanceof params.Parent)
        return true;
      return inst?._zod?.traits?.has(name);
    }
  });
  Object.defineProperty(_, "name", { value: name });
  return _;
}
var $ZodAsyncError = class extends Error {
  constructor() {
    super(`Encountered Promise during synchronous parse. Use .parseAsync() instead.`);
  }
};
(_a = globalThis).__zod_globalConfig ?? (_a.__zod_globalConfig = {});
var globalConfig = globalThis.__zod_globalConfig;
function config(newConfig) {
  if (newConfig)
    Object.assign(globalConfig, newConfig);
  return globalConfig;
}

// scripts/moq-browser/node_modules/zod/v4/core/errors.js
function _getMessage() {
  const internals = this._zod;
  internals.message ?? (internals.message = JSON.stringify(internals.def, jsonStringifyReplacer, 2));
  return internals.message;
}
function _setMessage(value) {
  this._zod.message = value;
}
var _messageDesc = {
  get: _getMessage,
  set: _setMessage,
  enumerable: true,
  configurable: true
};
var _issuesDesc = { value: void 0, enumerable: false };
var _installedToString = /* @__PURE__ */ new WeakSet([Object.prototype, Error.prototype]);
var initializer = (inst, def) => {
  inst.name = "$ZodError";
  _issuesDesc.value = def;
  Object.defineProperty(inst, "issues", _issuesDesc);
  _issuesDesc.value = void 0;
  Object.defineProperty(inst, "message", _messageDesc);
  const proto = Object.getPrototypeOf(inst);
  if (!_installedToString.has(proto)) {
    _installedToString.add(proto);
    Object.defineProperty(proto, "toString", {
      configurable: true,
      enumerable: false,
      get() {
        const value = () => this.message;
        Object.defineProperty(this, "toString", { value, configurable: true, writable: true });
        return value;
      },
      set(value) {
        Object.defineProperty(this, "toString", { value, configurable: true, writable: true });
      }
    });
  }
};
var $ZodError = $constructor("$ZodError", initializer);
var $ZodRealError = $constructor("$ZodError", initializer, void 0, {
  Parent: Error
});

// scripts/moq-browser/node_modules/zod/v4/core/parse.js
var _parse = (_Err) => {
  const fn = (schema, value, _ctx, _params) => {
    const ctx = _ctx ? { ..._ctx, async: false } : { async: false };
    const result = schema._zod.run({ value, issues: [] }, ctx);
    if (result instanceof Promise) {
      throw new $ZodAsyncError();
    }
    if (result.issues.length) {
      const e = new (_params?.Err ?? _Err)(result.issues.map((iss) => finalizeIssue(iss, ctx, config())));
      captureStackTrace(e, _params?.callee ?? fn);
      throw e;
    }
    return result.value;
  };
  return fn;
};
var parse = /* @__PURE__ */ _parse($ZodRealError);
var _parseAsync = (_Err) => {
  const fn = async (schema, value, _ctx, params) => {
    const ctx = _ctx ? { ..._ctx, async: true } : { async: true };
    let result = schema._zod.run({ value, issues: [] }, ctx);
    if (result instanceof Promise)
      result = await result;
    if (result.issues.length) {
      const e = new (params?.Err ?? _Err)(result.issues.map((iss) => finalizeIssue(iss, ctx, config())));
      captureStackTrace(e, params?.callee ?? fn);
      throw e;
    }
    return result.value;
  };
  return fn;
};
var parseAsync = /* @__PURE__ */ _parseAsync($ZodRealError);
var _safeParse = (_Err) => (schema, value, _ctx) => {
  const ctx = _ctx ? { ..._ctx, async: false } : { async: false };
  const result = schema._zod.run({ value, issues: [] }, ctx);
  if (result instanceof Promise) {
    throw new $ZodAsyncError();
  }
  return result.issues.length ? failure(_Err, result.issues, ctx) : { success: true, data: result.value };
};
var safeParse = /* @__PURE__ */ _safeParse($ZodRealError);
function failure(Err, issues, ctx) {
  let error2;
  return {
    success: false,
    get error() {
      if (!error2) {
        error2 = new Err(issues.map((iss) => finalizeIssue(iss, ctx, config())));
        issues = void 0;
        ctx = void 0;
      }
      return error2;
    },
    set error(e) {
      error2 = e;
      issues = void 0;
      ctx = void 0;
    }
  };
}
var _safeParseAsync = (_Err) => async (schema, value, _ctx) => {
  const ctx = _ctx ? { ..._ctx, async: true } : { async: true };
  let result = schema._zod.run({ value, issues: [] }, ctx);
  if (result instanceof Promise)
    result = await result;
  return result.issues.length ? failure(_Err, result.issues, ctx) : { success: true, data: result.value };
};
var safeParseAsync = /* @__PURE__ */ _safeParseAsync($ZodRealError);

// scripts/moq-browser/node_modules/zod/v4/core/regexes.js
var bigint = /^-?\d+n?$/;

// scripts/moq-browser/node_modules/zod/v4/core/checks.js
var $ZodCheck = /* @__PURE__ */ $constructor("$ZodCheck", (inst, def) => {
  var _a2;
  inst._zod ?? (inst._zod = {});
  inst._zod.def = def;
  (_a2 = inst._zod).onattach ?? (_a2.onattach = []);
});

// scripts/moq-browser/node_modules/zod/v4/core/versions.js
var version = {
  major: 4,
  minor: 6,
  patch: 5
};

// scripts/moq-browser/node_modules/zod/v4/core/schemas.js
var $ZodType = /* @__PURE__ */ $constructor("$ZodType", (inst, def) => {
  var _a2;
  inst ?? (inst = {});
  inst._zod.def = def;
  inst._zod.bag = inst._zod.bag || {};
  inst._zod.version = version;
  const defChecks = inst._zod.def.checks;
  const checks = inst._zod.traits.has("$ZodCheck") ? [inst, ...defChecks ?? []] : defChecks?.length ? [...defChecks] : [];
  for (const ch of checks) {
    for (const fn of ch._zod.onattach) {
      fn(inst);
    }
  }
  if (checks.length === 0) {
    (_a2 = inst._zod).deferred ?? (_a2.deferred = []);
    inst._zod.deferred?.push(() => {
      inst._zod.run = inst._zod.parse;
    });
  } else {
    const runChecks = (payload, checks2, ctx) => {
      if (payload.memo)
        return payload;
      let isAborted = aborted(payload);
      let asyncResult;
      for (const ch of checks2) {
        if (ch._zod.def.when) {
          if (explicitlyAborted(payload))
            continue;
          const shouldRun = ch._zod.def.when(payload);
          if (!shouldRun)
            continue;
        } else if (isAborted) {
          continue;
        }
        const currLen = payload.issues.length;
        const _ = ch._zod.check(payload);
        if (_ instanceof Promise && ctx?.async === false) {
          throw new $ZodAsyncError();
        }
        if (asyncResult || _ instanceof Promise) {
          asyncResult = (asyncResult ?? Promise.resolve()).then(async () => {
            await _;
            const nextLen = payload.issues.length;
            if (nextLen === currLen)
              return;
            attachSchema(payload.issues, currLen, inst);
            if (!isAborted)
              isAborted = aborted(payload, currLen);
          });
        } else {
          const nextLen = payload.issues.length;
          if (nextLen === currLen)
            continue;
          attachSchema(payload.issues, currLen, inst);
          if (!isAborted)
            isAborted = aborted(payload, currLen);
        }
      }
      if (asyncResult) {
        return asyncResult.then(() => {
          return payload;
        });
      }
      return payload;
    };
    const handleCanaryResult = (canary, payload, ctx) => {
      if (aborted(canary)) {
        canary.aborted = true;
        return canary;
      }
      const checkResult = runChecks(payload, checks, ctx);
      if (checkResult instanceof Promise) {
        if (ctx.async === false)
          throw new $ZodAsyncError();
        return checkResult.then((checkResult2) => inst._zod.parse(checkResult2, ctx));
      }
      return inst._zod.parse(checkResult, ctx);
    };
    inst._zod.run = (payload, ctx) => {
      if (ctx.skipChecks) {
        return inst._zod.parse(payload, ctx);
      }
      if (ctx.direction === "backward") {
        const canary = inst._zod.parse({ value: payload.value, issues: [] }, { ...ctx, skipChecks: true });
        if (canary instanceof Promise) {
          return canary.then((canary2) => {
            return handleCanaryResult(canary2, payload, ctx);
          });
        }
        return handleCanaryResult(canary, payload, ctx);
      }
      const result = inst._zod.parse(payload, ctx);
      if (result instanceof Promise) {
        if (ctx.async === false)
          throw new $ZodAsyncError();
        return result.then((result2) => runChecks(result2, checks, ctx));
      }
      return runChecks(result, checks, ctx);
    };
  }
}, {
  // Wrappers extend this by installing a richer factory over it; reading it eagerly would defeat the laziness.
  get "~standard"() {
    return hide(this, "~standard", standardProps(this));
  },
  set "~standard"(value) {
    own(this, "~standard", value);
  }
});
var toStandardResult = (r, ctx) => r.issues.length ? { issues: r.issues.map((iss) => finalizeIssue(iss, ctx, config())) } : { value: r.value };
async function validateAsync(inst, value) {
  const ctx = { async: true };
  return toStandardResult(await inst._zod.run({ value, issues: [] }, ctx), ctx);
}
function standardProps(inst) {
  return {
    validate: (value) => {
      const ctx = { async: false };
      try {
        const r = inst._zod.run({ value, issues: [] }, ctx);
        if (!(r instanceof Promise))
          return toStandardResult(r, ctx);
      } catch (_) {
      }
      return validateAsync(inst, value);
    },
    vendor: "zod",
    version: 1
  };
}
var $ZodBigInt = /* @__PURE__ */ $constructor("$ZodBigInt", (inst, def) => {
  $ZodType.init(inst, def);
  inst._zod.pattern = bigint;
  inst._zod.parse = (payload, _ctx) => {
    if (def.coerce)
      try {
        payload.value = BigInt(payload.value);
      } catch (_) {
      }
    if (typeof payload.value === "bigint")
      return payload;
    payload.issues.push({
      expected: "bigint",
      code: "invalid_type",
      input: payload.value,
      inst
    });
    return payload;
  };
});
var $ZodCustom = /* @__PURE__ */ $constructor("$ZodCustom", (inst, def) => {
  $ZodCheck.init(inst, def);
  $ZodType.init(inst, def);
  inst._zod.parse = (payload, _) => {
    return payload;
  };
  inst._zod.check = (payload) => {
    const input = payload.value;
    const r = def.fn(input);
    if (r instanceof Promise) {
      return r.then((r2) => handleRefineResult(r2, payload, input, inst));
    }
    handleRefineResult(r, payload, input, inst);
    return;
  };
});
function handleRefineResult(result, payload, input, inst) {
  if (!result) {
    const _iss = {
      code: "custom",
      input,
      inst,
      // incorporates params.error into issue reporting
      path: [...inst._zod.def.path ?? []],
      // incorporates params.error into issue reporting
      continue: !inst._zod.def.abort
      // params: inst._zod.def.params,
    };
    if (inst._zod.def.params)
      _iss.params = inst._zod.def.params;
    payload.issues.push(issue(_iss));
  }
}

// scripts/moq-browser/node_modules/zod/v4/core/api.js
// @__NO_SIDE_EFFECTS__
function _bigint(Class, params) {
  return new Class({
    type: "bigint",
    ...normalizeParams(params)
  });
}
// @__NO_SIDE_EFFECTS__
function _refine(Class, fn, _params) {
  const schema = new Class({
    type: "custom",
    check: "custom",
    fn,
    ...normalizeParams(_params)
  });
  return schema;
}

// scripts/moq-browser/node_modules/zod/v4/mini/schemas.js
var ZodMiniType = /* @__PURE__ */ $constructor("ZodMiniType", (inst, def) => {
  if (!inst._zod)
    throw new Error("Uninitialized schema in ZodMiniType.");
  $ZodType.init(inst, def);
  inst.def = def;
  inst.type = def.type;
}, {
  // `with` is an alias for `check`: the same function object, not a wrapper.
  get with() {
    return this.check;
  },
  set with(value) {
    own(this, "with", value);
  },
  parse(data, params) {
    return parse(this, data, params, { callee: this.parse });
  },
  parseAsync(data, params) {
    return parseAsync(this, data, params, { callee: this.parseAsync });
  },
  safeParse(data, params) {
    return safeParse(this, data, params);
  },
  safeParseAsync(data, params) {
    return safeParseAsync(this, data, params);
  },
  check(...checks) {
    const def = this.def;
    return this.clone({
      ...def,
      checks: [
        ...def.checks ?? [],
        ...checks.map((ch) => typeof ch === "function" ? { _zod: { check: ch, def: { check: "custom" }, onattach: [] } } : ch)
      ]
    }, { parent: true });
  },
  clone(_def, params) {
    return clone(this, _def, params);
  },
  brand() {
    return this;
  },
  register(reg, meta2) {
    reg.add(this, meta2);
    return this;
  },
  apply(fn, ...args) {
    return args.length === 0 ? fn(this) : fn(this, ...args);
  }
});
var ZodMiniBigInt = /* @__PURE__ */ $constructor("ZodMiniBigInt", (inst, def) => {
  $ZodBigInt.init(inst, def);
  ZodMiniType.init(inst, def);
});
// @__NO_SIDE_EFFECTS__
function bigint2(params) {
  return _bigint(ZodMiniBigInt, params);
}
var ZodMiniCustom = /* @__PURE__ */ $constructor("ZodMiniCustom", (inst, def) => {
  $ZodCustom.init(inst, def);
  ZodMiniType.init(inst, def);
});
// @__NO_SIDE_EFFECTS__
function refine(fn, _params = {}) {
  return _refine(ZodMiniCustom, fn, _params);
}

// vendor/moq/js/net/src/origin.ts
var OriginSchema = bigint2().check(refine((value) => value >= 0n && value < 1n << 62n, "Origin must be a non-negative 62-bit integer")).brand("Origin");
var UNKNOWN_ORIGIN = OriginSchema.parse(0n);
var MAX_HOPS = 32;
function randomOrigin() {
  const buf = new BigUint64Array(1);
  crypto.getRandomValues(buf);
  const raw = buf[0] & 0x1fffffffffffffn;
  return OriginSchema.parse(raw === 0n ? 1n : raw);
}

// vendor/moq/js/net/src/ietf/parameters.ts
var SetupOption = {
  Path: 1n,
  MaxRequestId: 2n,
  AuthorizationToken: 3n,
  MaxAuthTokenCacheSize: 4n,
  Authority: 5n,
  Implementation: 7n,
  /** RELAY_HOPS, from the MoQ Cluster extension. See `cluster.ts`. */
  RelayHops: 0x40b55n,
  /** RELAY_COST, from the MoQ Cluster extension. See `cluster.ts`. */
  RelayCost: 0x40b56n,
  /** SOLICIT, from the MoQ Solicit extension. See `solicit.ts`. */
  Solicit: 0x40b5an
};
var MAX_AUTHORIZATION_BYTES = 8 * 1024;
function copyAuthorization(authorization) {
  if (authorization.byteLength > MAX_AUTHORIZATION_BYTES) {
    throw new RangeError(`authorization payload exceeds ${MAX_AUTHORIZATION_BYTES} bytes`);
  }
  return new Uint8Array(authorization);
}
function setAuthorization(options, authorization) {
  const proof = copyAuthorization(authorization);
  const token = new Uint8Array(2 + proof.byteLength);
  token[0] = 3;
  token[1] = 0;
  token.set(proof, 2);
  options.setBytes(SetupOption.AuthorizationToken, token);
}
var SetupOptions = class _SetupOptions {
  vars;
  bytes;
  constructor() {
    this.vars = /* @__PURE__ */ new Map();
    this.bytes = /* @__PURE__ */ new Map();
  }
  get size() {
    return this.vars.size + this.bytes.size;
  }
  setBytes(id, value) {
    if (id % 2n !== 1n) {
      throw new Error(`invalid parameter id: ${id.toString()}, must be odd`);
    }
    this.bytes.set(id, value);
  }
  setVarint(id, value) {
    if (id % 2n !== 0n) {
      throw new Error(`invalid parameter id: ${id.toString()}, must be even`);
    }
    this.vars.set(id, value);
  }
  getBytes(id) {
    if (id % 2n !== 1n) {
      throw new Error(`invalid parameter id: ${id.toString()}, must be odd`);
    }
    return this.bytes.get(id);
  }
  getVarint(id) {
    if (id % 2n !== 0n) {
      throw new Error(`invalid parameter id: ${id.toString()}, must be even`);
    }
    return this.vars.get(id);
  }
  removeBytes(id) {
    if (id % 2n !== 1n) {
      throw new Error(`invalid parameter id: ${id.toString()}, must be odd`);
    }
    return this.bytes.delete(id);
  }
  removeVarint(id) {
    if (id % 2n !== 0n) {
      throw new Error(`invalid parameter id: ${id.toString()}, must be even`);
    }
    return this.vars.delete(id);
  }
  async encode(w, version2) {
    if (version2 !== Version.DRAFT_14 && version2 !== Version.DRAFT_15) {
      if (version2 === Version.DRAFT_16) {
        await w.u53(this.vars.size + this.bytes.size);
      }
      const all = [];
      for (const id of this.vars.keys()) all.push({ key: id, isVar: true });
      for (const id of this.bytes.keys()) all.push({ key: id, isVar: false });
      all.sort((a, b) => a.key < b.key ? -1 : a.key > b.key ? 1 : 0);
      let prevId = 0n;
      for (let i = 0; i < all.length; i++) {
        const { key, isVar } = all[i];
        const delta = i === 0 ? key : key - prevId;
        prevId = key;
        await w.u62(delta);
        if (isVar) {
          await w.u62(this.vars.get(key));
        } else {
          const value = this.bytes.get(key);
          await w.u53(value.length);
          await w.write(value);
        }
      }
    } else {
      await w.u53(this.vars.size + this.bytes.size);
      for (const [id, value] of this.vars) {
        await w.u62(id);
        await w.u62(value);
      }
      for (const [id, value] of this.bytes) {
        await w.u62(id);
        await w.u53(value.length);
        await w.write(value);
      }
    }
  }
  static async decode(r, version2) {
    const params = new _SetupOptions();
    if (version2 !== Version.DRAFT_14 && version2 !== Version.DRAFT_15 && version2 !== Version.DRAFT_16) {
      let prevType = 0n;
      let i = 0;
      while (!await r.done()) {
        const delta = await r.u62();
        const id = i === 0 ? delta : prevType + delta;
        prevType = id;
        i++;
        if (id % 2n === 0n) {
          if (params.vars.has(id)) {
            throw new Error(`duplicate parameter id: ${id.toString()}`);
          }
          const varint2 = await r.u62();
          params.setVarint(id, varint2);
        } else {
          if (params.bytes.has(id)) {
            throw new Error(`duplicate parameter id: ${id.toString()}`);
          }
          const size2 = await r.u53();
          const bytes = await r.read(size2);
          params.setBytes(id, bytes);
        }
      }
    } else {
      const count = await r.u53();
      let prevType = 0n;
      for (let i = 0; i < count; i++) {
        let id;
        if (version2 === Version.DRAFT_16) {
          const delta = await r.u62();
          id = i === 0 ? delta : prevType + delta;
          prevType = id;
        } else {
          id = await r.u62();
        }
        if (id % 2n === 0n) {
          if (params.vars.has(id)) {
            throw new Error(`duplicate parameter id: ${id.toString()}`);
          }
          const varint2 = await r.u62();
          params.setVarint(id, varint2);
        } else {
          if (params.bytes.has(id)) {
            throw new Error(`duplicate parameter id: ${id.toString()}`);
          }
          const size2 = await r.u53();
          const bytes = await r.read(size2);
          params.setBytes(id, bytes);
        }
      }
    }
    return params;
  }
};
var MSG_PARAM_DELIVERY_TIMEOUT = 0x02n;
var MSG_PARAM_SUBGROUP_DELIVERY_TIMEOUT = 0x06n;
var MSG_PARAM_MAX_CACHE_DURATION = 0x04n;
var MSG_PARAM_EXPIRES = 0x08n;
var MSG_PARAM_PUBLISHER_PRIORITY = 0x0en;
var MSG_PARAM_FORWARD = 0x10n;
var MSG_PARAM_SUBSCRIBER_PRIORITY = 0x20n;
var MSG_PARAM_GROUP_ORDER = 0x22n;
var MSG_PARAM_ROUTE_COST = 0x40b58n;
var MSG_PARAM_LARGEST_OBJECT = 0x09n;
var MSG_PARAM_SUBSCRIPTION_FILTER = 0x21n;
var MSG_PARAM_FILL_PARAMETERS = 0x23n;
var MSG_PARAM_INCLUDE_PROPERTIES = 0x35n;
var MSG_PARAM_HOP_PATH = 0x40b57n;
function getMessageParamKind(id) {
  switch (id) {
    case MSG_PARAM_DELIVERY_TIMEOUT:
    case MSG_PARAM_SUBGROUP_DELIVERY_TIMEOUT:
    case MSG_PARAM_MAX_CACHE_DURATION:
    case MSG_PARAM_EXPIRES:
    case MSG_PARAM_ROUTE_COST:
      return "varint";
    case MSG_PARAM_PUBLISHER_PRIORITY:
    case MSG_PARAM_SUBSCRIBER_PRIORITY:
    case MSG_PARAM_GROUP_ORDER:
      return "uint8";
    case MSG_PARAM_FORWARD:
      return "bool";
    case MSG_PARAM_LARGEST_OBJECT:
      return "location";
    case MSG_PARAM_SUBSCRIPTION_FILTER:
    case MSG_PARAM_FILL_PARAMETERS:
    case MSG_PARAM_INCLUDE_PROPERTIES:
    case MSG_PARAM_HOP_PATH:
      return "bytes";
    default:
      throw new Error(`unknown message parameter id: ${id.toString()}`);
  }
}
function decodeLocation(data) {
  const [groupId, objectData] = decodeBigInt(data);
  const [objectId, trailing] = decodeBigInt(objectData);
  if (trailing.length !== 0) {
    throw new Error("trailing bytes in message parameter Location");
  }
  return { groupId, objectId };
}
function encodeLocation({ groupId, objectId }) {
  const group = encode2(groupId);
  const object = encode2(objectId);
  const combined = new Uint8Array(group.length + object.length);
  combined.set(group, 0);
  combined.set(object, group.length);
  return combined;
}
var Parameters = class _Parameters {
  vars;
  bytes;
  #locations;
  constructor() {
    this.vars = /* @__PURE__ */ new Map();
    this.bytes = /* @__PURE__ */ new Map();
    this.#locations = /* @__PURE__ */ new Map();
  }
  // --- Numeric accessors ---
  get subscriberPriority() {
    const v = this.vars.get(MSG_PARAM_SUBSCRIBER_PRIORITY);
    return v !== void 0 ? Number(v) : void 0;
  }
  set subscriberPriority(v) {
    this.vars.set(MSG_PARAM_SUBSCRIBER_PRIORITY, BigInt(v));
  }
  get groupOrder() {
    const v = this.vars.get(MSG_PARAM_GROUP_ORDER);
    return v !== void 0 ? Number(v) : void 0;
  }
  set groupOrder(v) {
    this.vars.set(MSG_PARAM_GROUP_ORDER, BigInt(v));
  }
  get forward() {
    const v = this.vars.get(MSG_PARAM_FORWARD);
    return v !== void 0 ? v !== 0n : void 0;
  }
  set forward(v) {
    this.vars.set(MSG_PARAM_FORWARD, v ? 1n : 0n);
  }
  get publisherPriority() {
    const v = this.vars.get(MSG_PARAM_PUBLISHER_PRIORITY);
    return v !== void 0 ? Number(v) : void 0;
  }
  set publisherPriority(v) {
    this.vars.set(MSG_PARAM_PUBLISHER_PRIORITY, BigInt(v));
  }
  get expires() {
    return this.vars.get(MSG_PARAM_EXPIRES);
  }
  set expires(v) {
    this.vars.set(MSG_PARAM_EXPIRES, v);
  }
  get deliveryTimeout() {
    return this.vars.get(MSG_PARAM_DELIVERY_TIMEOUT);
  }
  set deliveryTimeout(v) {
    this.vars.set(MSG_PARAM_DELIVERY_TIMEOUT, v);
  }
  get maxCacheDuration() {
    return this.vars.get(MSG_PARAM_MAX_CACHE_DURATION);
  }
  set maxCacheDuration(v) {
    this.vars.set(MSG_PARAM_MAX_CACHE_DURATION, v);
  }
  // --- Bytes accessors ---
  get largest() {
    const location = this.#locations.get(MSG_PARAM_LARGEST_OBJECT);
    return location && { ...location };
  }
  set largest(v) {
    this.#locations.set(MSG_PARAM_LARGEST_OBJECT, { ...v });
  }
  /**
   * LOCATION_FILTER, as its raw parameter value.
   *
   * Raw because the encoding is version dependent: a Filter Type tag through draft-19, a
   * list of up to four varints from draft-20 on. See `filter.ts`.
   */
  get subscriptionFilter() {
    return this.bytes.get(MSG_PARAM_SUBSCRIPTION_FILTER);
  }
  set subscriptionFilter(v) {
    this.bytes.set(MSG_PARAM_SUBSCRIPTION_FILTER, v);
  }
  /** FILL_PARAMETERS: the draft-20 backfill request, as its raw parameter value. */
  get fillParameters() {
    return this.bytes.get(MSG_PARAM_FILL_PARAMETERS);
  }
  set fillParameters(v) {
    this.bytes.set(MSG_PARAM_FILL_PARAMETERS, v);
  }
  /** INCLUDE_PROPERTIES: whether the peer wants Track Properties on the response. */
  get includeProperties() {
    const data = this.bytes.get(MSG_PARAM_INCLUDE_PROPERTIES);
    if (!data) return void 0;
    if (data.length !== 1 || data[0] > 1) {
      throw new Error(`invalid INCLUDE_PROPERTIES value: ${data.join(",")}`);
    }
    return data[0] === 1;
  }
  set includeProperties(v) {
    this.bytes.set(MSG_PARAM_INCLUDE_PROPERTIES, new Uint8Array([v ? 1 : 0]));
  }
  /** HOP_PATH: the hop chain an advertisement traversed, as its raw parameter value. */
  get hopPath() {
    return this.bytes.get(MSG_PARAM_HOP_PATH);
  }
  set hopPath(v) {
    this.bytes.set(MSG_PARAM_HOP_PATH, v);
  }
  /** ROUTE_COST: the accumulated cost of that path. Absent means 0. */
  get routeCost() {
    return this.vars.get(MSG_PARAM_ROUTE_COST);
  }
  set routeCost(v) {
    this.vars.set(MSG_PARAM_ROUTE_COST, v);
  }
  async encode(w, version2) {
    await w.u53(this.vars.size + this.bytes.size + this.#locations.size);
    if (version2 === Version.DRAFT_14 || version2 === Version.DRAFT_15) {
      for (const [id, value] of this.vars) {
        await w.u62(id);
        await w.u62(value);
      }
      for (const [id, value] of this.bytes) {
        await w.u62(id);
        await w.u53(value.length);
        await w.write(value);
      }
      for (const [id, value] of this.#locations) {
        const encoded = encodeLocation(value);
        await w.u62(id);
        await w.u53(encoded.length);
        await w.write(encoded);
      }
    } else {
      const all = [];
      for (const id of this.vars.keys()) all.push({ key: id, storage: "var" });
      for (const id of this.bytes.keys()) all.push({ key: id, storage: "bytes" });
      for (const id of this.#locations.keys()) all.push({ key: id, storage: "location" });
      all.sort((a, b) => a.key < b.key ? -1 : a.key > b.key ? 1 : 0);
      let prevId = 0n;
      for (let i = 0; i < all.length; i++) {
        const { key, storage } = all[i];
        const delta = i === 0 ? key : key - prevId;
        prevId = key;
        await w.u62(delta);
        if (version2 === Version.DRAFT_16) {
          if (storage === "var") {
            await w.u62(this.vars.get(key));
          } else {
            const value = storage === "bytes" ? (
              // biome-ignore lint/style/noNonNullAssertion: key is guaranteed to exist in bytes map
              this.bytes.get(key)
            ) : (
              // biome-ignore lint/style/noNonNullAssertion: key is guaranteed to exist in locations map
              encodeLocation(this.#locations.get(key))
            );
            await w.u53(value.length);
            await w.write(value);
          }
          continue;
        }
        switch (getMessageParamKind(key)) {
          case "varint": {
            const value = this.vars.get(key);
            if (value === void 0) throw new Error(`invalid varint message parameter: ${key.toString()}`);
            await w.u62(value);
            break;
          }
          case "uint8": {
            const value = this.vars.get(key);
            if (value === void 0 || value < 0n || value > 0xffn) {
              throw new Error(`invalid uint8 message parameter: ${key.toString()}`);
            }
            await w.u8(Number(value));
            break;
          }
          case "bool": {
            const value = this.vars.get(key);
            if (value !== 0n && value !== 1n) {
              throw new Error(`invalid bool message parameter: ${key.toString()}`);
            }
            await w.bool(value === 1n);
            break;
          }
          case "location": {
            const location = this.#locations.get(key);
            if (location === void 0)
              throw new Error(`invalid Location message parameter: ${key.toString()}`);
            await w.u62(location.groupId);
            await w.u62(location.objectId);
            break;
          }
          case "bytes": {
            const value = this.bytes.get(key);
            if (value === void 0) throw new Error(`invalid bytes message parameter: ${key.toString()}`);
            await w.u53(value.length);
            await w.write(value);
            break;
          }
        }
      }
    }
  }
  static async decode(r, version2) {
    const count = await r.u53();
    const params = new _Parameters();
    let prevType = 0n;
    for (let i = 0; i < count; i++) {
      let id;
      if (version2 === Version.DRAFT_14 || version2 === Version.DRAFT_15) {
        id = await r.u62();
      } else {
        const delta = await r.u62();
        id = i === 0 ? delta : prevType + delta;
        prevType = id;
      }
      if (version2 === Version.DRAFT_14 || version2 === Version.DRAFT_15 || version2 === Version.DRAFT_16) {
        if (id % 2n === 0n) {
          if (params.vars.has(id)) {
            throw new Error(`duplicate message parameter id: ${id.toString()}`);
          }
          const varint2 = await r.u62();
          params.vars.set(id, varint2);
        } else {
          const size2 = await r.u53();
          const bytes = await r.read(size2);
          if (id === MSG_PARAM_LARGEST_OBJECT) {
            if (params.#locations.has(id)) {
              throw new Error(`duplicate message parameter id: ${id.toString()}`);
            }
            params.#locations.set(id, decodeLocation(bytes));
          } else {
            if (params.bytes.has(id)) {
              throw new Error(`duplicate message parameter id: ${id.toString()}`);
            }
            params.bytes.set(id, bytes);
          }
        }
        continue;
      }
      if (params.vars.has(id) || params.bytes.has(id) || params.#locations.has(id)) {
        throw new Error(`duplicate message parameter id: ${id.toString()}`);
      }
      switch (getMessageParamKind(id)) {
        case "varint":
          params.vars.set(id, await r.u62());
          break;
        case "uint8":
          params.vars.set(id, BigInt(await r.u8()));
          break;
        case "bool":
          params.vars.set(id, await r.bool() ? 1n : 0n);
          break;
        case "location": {
          const groupId = await r.u62();
          const objectId = await r.u62();
          params.#locations.set(id, { groupId, objectId });
          break;
        }
        case "bytes": {
          const size2 = await r.u53();
          params.bytes.set(id, await r.read(size2));
          break;
        }
      }
    }
    return params;
  }
};

// vendor/moq/js/net/src/ietf/cluster.ts
function supported(version2) {
  return version2 !== Version.DRAFT_14 && version2 !== Version.DRAFT_15 && version2 !== Version.DRAFT_16;
}
function fromSetup(params, version2) {
  if (!supported(version2)) return void 0;
  const value = params.getBytes(SetupOption.RelayHops);
  if (value === void 0) return void 0;
  const [origin, rest] = decodeLeadingOnes(value);
  if (rest.length !== 0) throw new Error("trailing bytes in RELAY_HOPS");
  return OriginSchema.parse(origin);
}
function intoSetup(params, self, version2) {
  if (!supported(version2)) return;
  params.setBytes(SetupOption.RelayHops, encodeLeadingOnes(self));
}
function negotiated(ids) {
  return ids?.peer !== void 0;
}
function advertise(ids) {
  if (ids === void 0 || ids.peer === void 0) return void 0;
  return { hops: [ids.self], cost: 0n };
}
function loops(advert, self) {
  return self !== UNKNOWN_ORIGIN && advert.hops.includes(self);
}
function intoParams(advert) {
  validate2(advert.hops);
  const params = new Parameters();
  const hops = advert.hops.map((hop) => encodeLeadingOnes(hop));
  const value = new Uint8Array(hops.reduce((total, hop) => total + hop.length, 0));
  let offset = 0;
  for (const hop of hops) {
    value.set(hop, offset);
    offset += hop.length;
  }
  params.hopPath = value;
  if (advert.cost !== 0n) params.routeCost = advert.cost;
  return params;
}
async function decodeParams(r, version2) {
  let params;
  try {
    params = await Parameters.decode(r, version2);
  } catch (err) {
    throw new ProtocolViolation(reason(err), { cause: err });
  }
  return fromParams(params);
}
function fromParams(params) {
  try {
    const value = params.hopPath;
    if (value === void 0) throw new Error("advertisement is missing HOP_PATH");
    const hops = [];
    let rest = value;
    while (rest.length > 0) {
      const [hop, remain] = decodeLeadingOnes(rest);
      hops.push(OriginSchema.parse(hop));
      rest = remain;
      if (hops.length > MAX_HOPS) throw new Error(`hop count exceeds maximum ${MAX_HOPS}`);
    }
    validate2(hops);
    return { hops, cost: params.routeCost ?? 0n };
  } catch (err) {
    throw new ProtocolViolation(reason(err), { cause: err });
  }
}
function validate2(hops) {
  if (hops.length === 0) throw new Error("hop path is empty");
  if (hops.length > MAX_HOPS) throw new Error(`hop count ${hops.length} exceeds maximum ${MAX_HOPS}`);
  for (let i = 0; i < hops.length; i++) {
    const hop = hops[i];
    if (hop === UNKNOWN_ORIGIN) continue;
    if (hops.indexOf(hop, i + 1) !== -1) throw new Error(`hop ${hop} appears twice in the hop path`);
  }
}

// vendor/moq/js/net/src/connection/stats.ts
async function transportStats(quic) {
  const getStats = quic.getStats;
  if (typeof getStats !== "function") return {};
  try {
    const stats = await getStats.call(quic);
    return {
      rtt: stats.smoothedRtt !== void 0 ? Milli(stats.smoothedRtt) : void 0,
      estimatedSendRate: stats.estimatedSendRate ?? void 0,
      bytesSent: stats.bytesSent,
      bytesReceived: stats.bytesReceived,
      bytesLost: stats.bytesLost,
      packetsSent: stats.packetsSent,
      packetsReceived: stats.packetsReceived,
      packetsLost: stats.packetsLost
    };
  } catch {
    return {};
  }
}

// scripts/moq-browser/node_modules/@moq/web-socket-stream/index.js
var CONNECTING = 0;
var OPEN = 1;
var DEFAULT_HIGH_WATER_MARK = 64 * 1024;
function validCloseCode(code) {
  return code !== void 0 && (code === 1e3 || code >= 3e3 && code <= 4999);
}
function drain(ws, highWaterMark) {
  if (ws.bufferedAmount <= highWaterMark())
    return void 0;
  return (async () => {
    while (ws.bufferedAmount > highWaterMark()) {
      if (ws.readyState > OPEN)
        throw new Error("WebSocket is closing");
      await new Promise((resolve2) => setTimeout(resolve2, 10));
    }
  })();
}
var WebSocketStream = class _WebSocketStream {
  url;
  opened;
  closed;
  #ws;
  #highWaterMark;
  /** Dial `url`, or wrap a socket you already have (see {@link WebSocketStream.adopt}). */
  constructor(source, options = {}) {
    let ws;
    if (typeof source === "string") {
      const Ctor = options.webSocket ?? globalThis.WebSocket;
      if (!Ctor) {
        throw new Error("No WebSocket implementation found; pass options.webSocket");
      }
      ws = new Ctor(source, options.protocols);
      this.url = source;
    } else {
      ws = source;
      this.url = source.url ?? "";
    }
    ws.binaryType = "arraybuffer";
    this.#ws = ws;
    this.#highWaterMark = Math.max(1, Math.floor(options.highWaterMark ?? DEFAULT_HIGH_WATER_MARK));
    const opened = Promise.withResolvers();
    const closed = Promise.withResolvers();
    this.opened = opened.promise;
    this.closed = closed.promise;
    this.opened.catch(() => {
    });
    let controller;
    const readable = new ReadableStream({
      start: (c) => {
        controller = c;
      },
      cancel: () => ws.close()
    });
    const writable = new WritableStream({
      write: (chunk) => {
        ws.send(chunk);
        return drain(ws, () => this.#highWaterMark);
      },
      // A WebSocket has no half-close, so closing/aborting the writable closes
      // the whole socket. Without this, `writer.close()` would leave it open.
      close: () => ws.close(),
      abort: () => ws.close()
    });
    ws.onopen = () => {
      opened.resolve({ readable, writable, extensions: ws.extensions, protocol: ws.protocol });
    };
    ws.onmessage = (event) => {
      const data = event.data;
      if (typeof data === "string") {
        controller?.enqueue(data);
      } else if (data instanceof ArrayBuffer) {
        controller?.enqueue(new Uint8Array(data));
      } else if (ArrayBuffer.isView(data)) {
        const view = data;
        controller?.enqueue(new Uint8Array(view.buffer, view.byteOffset, view.byteLength));
      }
    };
    ws.onerror = () => {
      const err = new Error("WebSocket connection error");
      opened.reject(err);
      try {
        controller?.error(err);
      } catch {
      }
    };
    ws.onclose = (event) => {
      opened.reject(new Error("WebSocket closed before opening"));
      try {
        controller?.close();
      } catch {
      }
      closed.resolve({ closeCode: event.code ?? 1006, reason: event.reason ?? "" });
    };
    if (ws.readyState !== CONNECTING) {
      if (ws.readyState === OPEN) {
        opened.resolve({ readable, writable, extensions: ws.extensions, protocol: ws.protocol });
      } else {
        opened.reject(new Error("WebSocket is already closed"));
        try {
          controller?.close();
        } catch {
        }
        closed.resolve({ closeCode: 1006, reason: "" });
      }
    }
    const { signal } = options;
    if (signal) {
      if (signal.aborted)
        ws.close();
      else
        signal.addEventListener("abort", () => ws.close(), { once: true });
    }
  }
  /** Wrap a socket that already exists — typically one a server accepted from an
   *  HTTP upgrade (`Deno.upgradeWebSocket`, `ws`, ...), where there is no URL to
   *  dial and the handshake (including subprotocol selection) is already done.
   *
   *  Ownership transfers: this overwrites the socket's `onopen`/`onmessage`/
   *  `onerror`/`onclose` handlers, so adopt it before anything else reads from
   *  it — messages delivered before adoption are dropped by the platform, not
   *  buffered. A socket that is already open resolves {@link opened} without
   *  waiting for an `onopen` that has already fired (or was never going to).
   *
   *  `options.protocols` and `options.webSocket` are ignored; the socket exists. */
  static adopt(ws, options = {}) {
    return new _WebSocketStream(ws, options);
  }
  close(closeInfo = {}) {
    if (validCloseCode(closeInfo.closeCode)) {
      this.#ws.close(closeInfo.closeCode, closeInfo.reason);
    } else {
      this.#ws.close();
    }
  }
  /** The current write-backpressure high-water mark, in bytes. */
  get highWaterMark() {
    return this.#highWaterMark;
  }
  /** Resize the write-backpressure high-water mark at runtime (bytes).
   *
   * Set this to roughly the bandwidth-delay product (RTT × estimated
   * throughput): large enough to keep the socket busy, small enough that
   * queued bytes can still be reprioritized rather than committed to the OS
   * send buffer. Takes effect immediately, including for an in-progress drain.
   * Clamped to a minimum of 1 byte. */
  setHighWaterMark(bytes) {
    this.#highWaterMark = Math.max(1, Math.floor(bytes));
  }
};
function openWebSocketStream(url, options = {}) {
  const href = typeof url === "string" ? url : url.toString();
  const Native = globalThis.WebSocketStream;
  if (Native && Native !== WebSocketStream && !options.webSocket) {
    const native = new Native(href, { protocols: options.protocols, signal: options.signal });
    return {
      url: native.url,
      opened: native.opened.then((event) => ({
        ...event,
        readable: event.readable.pipeThrough(new TransformStream({
          transform(chunk, controller) {
            controller.enqueue(chunk instanceof ArrayBuffer ? new Uint8Array(chunk) : chunk);
          }
        }))
      })),
      closed: native.closed,
      close: (closeInfo) => native.close(closeInfo)
    };
  }
  return new WebSocketStream(href, options);
}

// scripts/moq-browser/node_modules/@moq/qmux/credit.js
function replenishWindow(consumed, currentMax, window2) {
  if (window2 === 0n)
    return null;
  if (currentMax - consumed <= window2 / 2n)
    return consumed + window2;
  return null;
}
var Credit = class {
  #used;
  #max;
  #released = 0n;
  #closed = false;
  #closeReason;
  #waiters = [];
  constructor(max) {
    this.#used = 0n;
    this.#max = max;
  }
  /** Try to claim up to `limit` units. Returns amount claimed (0n if none available). */
  tryClaim(limit) {
    if (limit === 0n)
      return 0n;
    const available = this.#max - this.#used;
    if (available <= 0n)
      return 0n;
    const claimed = limit < available ? limit : available;
    this.#used += claimed;
    return claimed;
  }
  /** Claim up to `limit` units, waiting until credit is available.
   *  Rejects if the credit has been closed. Returns 0n for zero-limit requests. */
  async claim(limit) {
    if (limit === 0n)
      return 0n;
    while (true) {
      if (this.#closed)
        throw this.#closeReason ?? new Error("closed");
      const claimed = this.tryClaim(limit);
      if (claimed > 0n)
        return claimed;
      await new Promise((resolve2, reject) => {
        this.#waiters.push({ resolve: resolve2, reject });
      });
    }
  }
  /** Return previously claimed credit (for rollback). */
  release(amount) {
    this.#used = this.#used > amount ? this.#used - amount : 0n;
    this.#wake();
  }
  /** Increase the max. Returns false if new_max < current max. */
  increaseMax(newMax) {
    if (newMax < this.#max)
      return false;
    if (newMax === this.#max)
      return true;
    this.#max = newMax;
    this.#wake();
    return true;
  }
  /** Close the credit, rejecting all pending and future `claim()` calls.
   *  `reason` (if provided) becomes the rejection error; the first reason wins. */
  close(reason2) {
    this.#closed = true;
    this.#closeReason ??= reason2 ?? new Error("closed");
    const waiters = this.#waiters;
    this.#waiters = [];
    for (const { reject } of waiters)
      reject(this.#closeReason);
  }
  /** Set used to max(used, value). Returns false if value > max (flow control violation). */
  receiveUpTo(value) {
    if (value > this.#max)
      return false;
    if (value > this.#used)
      this.#used = value;
    return true;
  }
  /** Report that `len` units have been consumed.
   *  Returns the new max if a window update should be sent, or null otherwise. */
  consume(len) {
    this.#released += len;
    if (this.#used + 2n * this.#released > this.#max) {
      const newMax = this.#max + this.#released;
      this.#max = newMax;
      this.#released = 0n;
      this.#wake();
      return newMax;
    }
    return null;
  }
  /** Get current available credit (max - used). */
  get available() {
    const avail = this.#max - this.#used;
    return avail > 0n ? avail : 0n;
  }
  /** Get the current max value. */
  get max() {
    return this.#max;
  }
  /** Get the current used value. */
  get used() {
    return this.#used;
  }
  #wake() {
    const waiters = this.#waiters;
    this.#waiters = [];
    for (const { resolve: resolve2 } of waiters)
      resolve2();
  }
};

// scripts/moq-browser/node_modules/@moq/qmux/error.js
var SessionError = class extends Error {
  source = "session";
  streamErrorCode = null;
  constructor(message, options) {
    super(message, options);
    this.name = "WebTransportError";
  }
};
var StreamError = class extends Error {
  source = "stream";
  streamErrorCode;
  constructor(code, message = "stream reset") {
    super(`${message}: ${code}`);
    this.name = "WebTransportError";
    this.streamErrorCode = code;
  }
};
function resetCode(reason2) {
  if (typeof reason2 !== "object" || reason2 === null)
    return 0;
  const { streamErrorCode } = reason2;
  if (typeof streamErrorCode !== "number")
    return 0;
  return streamErrorCode >>> 0;
}
function streamCode2(code) {
  return Number(BigInt.asUintN(32, code));
}

// scripts/moq-browser/node_modules/@moq/qmux/varint.js
var VarInt = class _VarInt {
  static MAX = (1n << 62n) - 1n;
  static MAX_SIZE = 8;
  value;
  constructor(value) {
    if (value < 0n || value > _VarInt.MAX) {
      throw new Error(`VarInt value out of range: ${value}`);
    }
    this.value = value;
  }
  static from(value) {
    return new _VarInt(BigInt(value));
  }
  size() {
    const x = this.value;
    if (x < 2n ** 6n)
      return 1;
    if (x < 2n ** 14n)
      return 2;
    if (x < 2n ** 30n)
      return 4;
    if (x < 2n ** 62n)
      return 8;
    throw new Error("VarInt value too large");
  }
  // Append to the provided buffer
  encode(dst) {
    const x = this.value;
    const size2 = this.size();
    if (dst.byteOffset + dst.byteLength + size2 > dst.buffer.byteLength) {
      throw new Error("destination buffer too small");
    }
    const view = new DataView(dst.buffer, dst.byteOffset + dst.byteLength, size2);
    if (size2 === 1) {
      view.setUint8(0, Number(x));
    } else if (size2 === 2) {
      view.setUint16(0, 1 << 14 | Number(x), false);
    } else if (size2 === 4) {
      view.setUint32(0, 2 << 30 | Number(x), false);
    } else if (size2 === 8) {
      view.setBigUint64(0, 3n << 62n | x, false);
    } else {
      throw new Error("VarInt value too large");
    }
    return new Uint8Array(dst.buffer, dst.byteOffset, dst.byteLength + size2);
  }
  static decode(buffer) {
    if (buffer.byteLength < 1) {
      throw new Error("Unexpected end of buffer");
    }
    const view = new DataView(buffer.buffer, buffer.byteOffset);
    const firstByte = view.getUint8(0);
    const tag = firstByte >> 6;
    let value;
    let bytesRead;
    switch (tag) {
      case 0:
        value = BigInt(firstByte & 63);
        bytesRead = 1;
        break;
      case 1:
        if (2 > buffer.length) {
          throw new Error("Unexpected end of buffer");
        }
        value = BigInt(view.getUint16(0, false) & 16383);
        bytesRead = 2;
        break;
      case 2:
        if (4 > buffer.length) {
          throw new Error("Unexpected end of buffer");
        }
        value = BigInt(view.getUint32(0, false) & 1073741823);
        bytesRead = 4;
        break;
      case 3:
        if (8 > buffer.length) {
          throw new Error("Unexpected end of buffer");
        }
        value = view.getBigUint64(0, false) & 0x3fffffffffffffffn;
        bytesRead = 8;
        break;
      default:
        throw new Error("Invalid VarInt tag");
    }
    const remaining = new Uint8Array(buffer.buffer, buffer.byteOffset + bytesRead, buffer.byteLength - bytesRead);
    return [new _VarInt(value), remaining];
  }
};

// scripts/moq-browser/node_modules/@moq/qmux/stream.js
var Dir = {
  Bi: 0,
  Uni: 1
};
var Id = class _Id {
  value;
  constructor(value) {
    this.value = value;
  }
  static create(id, dir, isServer) {
    let streamId = id << 2n;
    if (dir === Dir.Uni) {
      streamId |= 0x02n;
    }
    if (isServer) {
      streamId |= 0x01n;
    }
    return new _Id(VarInt.from(streamId));
  }
  get dir() {
    return (this.value.value & 0x02n) !== 0n ? Dir.Uni : Dir.Bi;
  }
  get serverInitiated() {
    return (this.value.value & 0x01n) !== 0n;
  }
  /** Returns the 0-based sequence index of this stream. */
  get index() {
    return this.value.value >> 2n;
  }
  canRecv(isServer) {
    if (this.dir === Dir.Uni) {
      return this.serverInitiated !== isServer;
    }
    return true;
  }
  canSend(isServer) {
    if (this.dir === Dir.Uni) {
      return this.serverInitiated === isServer;
    }
    return true;
  }
};

// scripts/moq-browser/node_modules/@moq/qmux/frame.js
var MAX_FRAME_SIZE = 16384;
var MAX_FRAME_PAYLOAD = MAX_FRAME_SIZE - 32;
var VARINT_MAX = [63n, 16383n, 1073741823n, 4611686018427387903n];
function maxLengthPrefixedPayload(available) {
  let best = 0n;
  for (const max of VARINT_MAX) {
    const width = BigInt(VarInt.from(max).size());
    if (available < width)
      break;
    const candidate = available - width < max ? available - width : max;
    if (VarInt.from(candidate).size() === Number(width) && candidate > best)
      best = candidate;
  }
  return best;
}
function maxStreamPayload(version2, budget, id, offset) {
  const header = BigInt(1 + id.value.size());
  if (version2 === "webtransport") {
    return budget > header ? budget - header : 0n;
  }
  const fixed = header + BigInt(VarInt.from(offset).size());
  return maxLengthPrefixedPayload(budget > fixed ? budget - fixed : 0n);
}
var DEFAULT_MAX_RECORD_SIZE = 16382n;
var DEFAULT_TRANSPORT_PARAMS = {
  maxIdleTimeout: 0n,
  initialMaxData: 0n,
  initialMaxStreamDataBidiLocal: 0n,
  initialMaxStreamDataBidiRemote: 0n,
  initialMaxStreamDataUni: 0n,
  initialMaxStreamsBidi: 0n,
  initialMaxStreamsUni: 0n,
  maxDatagramFrameSize: 0n,
  maxRecordSize: DEFAULT_MAX_RECORD_SIZE,
  resetStreamAt: false
};
function encode5(frame, version2 = "webtransport") {
  if (version2 === "webtransport") {
    return encodeWebTransport(frame);
  }
  return encodeQMux(frame);
}
function isQmux(version2) {
  return version2 === "qmux-00" || version2 === "qmux-01" || version2 === "qmux-02";
}
function usesRecords(version2) {
  return version2 === "qmux-01" || version2 === "qmux-02";
}
var QX_PING_REQUEST = 0x348c67529ef8c7bdn;
var QX_PING_RESPONSE = 0x348c67529ef8c7ben;
var RESET_STREAM_AT = 0x24n;
var MAX_RECORD_SIZE_ID = 0x0571c59429cd0845n;
var APPLICATION_PROTOCOLS_ID = 0x3d4f9c2a8b1e6075n;
var RESET_STREAM_AT_PARAM_ID = 0x1dn;
var RECOGNIZED_PARAM_IDS = /* @__PURE__ */ new Set([
  0x01n,
  0x04n,
  0x05n,
  0x06n,
  0x07n,
  0x08n,
  0x09n,
  0x20n,
  MAX_RECORD_SIZE_ID,
  APPLICATION_PROTOCOLS_ID,
  RESET_STREAM_AT_PARAM_ID
]);
function encodeWebTransport(frame) {
  switch (frame.type) {
    case "stream": {
      let buffer = new Uint8Array(new ArrayBuffer(1 + 8 + frame.data.length), 0, 1);
      buffer[0] = frame.fin ? 9 : 8;
      buffer = frame.id.value.encode(buffer);
      buffer = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength + frame.data.length);
      buffer.set(frame.data, buffer.byteLength - frame.data.length);
      return buffer;
    }
    case "reset_stream": {
      let buffer = new Uint8Array(new ArrayBuffer(1 + 8 + 8), 0, 1);
      buffer[0] = 4;
      buffer = frame.id.value.encode(buffer);
      buffer = frame.code.encode(buffer);
      return buffer;
    }
    case "stop_sending": {
      let buffer = new Uint8Array(new ArrayBuffer(1 + 8 + 8), 0, 1);
      buffer[0] = 5;
      buffer = frame.id.value.encode(buffer);
      buffer = frame.code.encode(buffer);
      return buffer;
    }
    // The legacy WebTransport format keeps the reason as the rest of the buffer
    // (no Frame Type / length fields); only the type byte differs.
    case "connection_close":
    case "application_close": {
      const body = new TextEncoder().encode(frame.reason);
      let buffer = new Uint8Array(new ArrayBuffer(1 + 8 + body.length), 0, 1);
      buffer[0] = frame.type === "application_close" ? 29 : 28;
      buffer = frame.code.encode(buffer);
      buffer = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength + body.length);
      buffer.set(body, buffer.byteLength - body.length);
      return buffer;
    }
    default:
      throw new Error("flow control frames are not supported in WebTransport version");
  }
}
function encodeQMux(frame) {
  switch (frame.type) {
    case "stream": {
      const frameType = VarInt.from(14 | (frame.fin ? 1 : 0));
      const offsetVi = VarInt.from(frame.offset ?? 0n);
      const lengthVi = VarInt.from(frame.data.length);
      const maxSize = 8 + 8 + 8 + 8 + frame.data.length;
      let buffer = new Uint8Array(new ArrayBuffer(maxSize), 0, 0);
      buffer = frameType.encode(buffer);
      buffer = frame.id.value.encode(buffer);
      buffer = offsetVi.encode(buffer);
      buffer = lengthVi.encode(buffer);
      buffer = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength + frame.data.length);
      buffer.set(frame.data, buffer.byteLength - frame.data.length);
      return buffer;
    }
    case "reset_stream": {
      const frameType = VarInt.from(4);
      const finalSize = VarInt.from(frame.finalSize);
      let buffer = new Uint8Array(new ArrayBuffer(8 + 8 + 8 + 8), 0, 0);
      buffer = frameType.encode(buffer);
      buffer = frame.id.value.encode(buffer);
      buffer = frame.code.encode(buffer);
      buffer = finalSize.encode(buffer);
      return buffer;
    }
    case "stop_sending": {
      const frameType = VarInt.from(5);
      let buffer = new Uint8Array(new ArrayBuffer(8 + 8 + 8), 0, 0);
      buffer = frameType.encode(buffer);
      buffer = frame.id.value.encode(buffer);
      buffer = frame.code.encode(buffer);
      return buffer;
    }
    case "connection_close": {
      const frameType = VarInt.from(28);
      const causingFrameType = VarInt.from(0);
      const body = new TextEncoder().encode(frame.reason);
      const reasonLength = VarInt.from(body.length);
      let buffer = new Uint8Array(new ArrayBuffer(8 + 8 + 8 + 8 + body.length), 0, 0);
      buffer = frameType.encode(buffer);
      buffer = frame.code.encode(buffer);
      buffer = causingFrameType.encode(buffer);
      buffer = reasonLength.encode(buffer);
      buffer = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength + body.length);
      buffer.set(body, buffer.byteLength - body.length);
      return buffer;
    }
    case "application_close": {
      const frameType = VarInt.from(29);
      const body = new TextEncoder().encode(frame.reason);
      const reasonLength = VarInt.from(body.length);
      let buffer = new Uint8Array(new ArrayBuffer(8 + 8 + 8 + body.length), 0, 0);
      buffer = frameType.encode(buffer);
      buffer = frame.code.encode(buffer);
      buffer = reasonLength.encode(buffer);
      buffer = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength + body.length);
      buffer.set(body, buffer.byteLength - body.length);
      return buffer;
    }
    case "max_data": {
      let buffer = new Uint8Array(new ArrayBuffer(16), 0, 0);
      buffer = VarInt.from(16).encode(buffer);
      buffer = VarInt.from(frame.max).encode(buffer);
      return buffer;
    }
    case "max_stream_data": {
      let buffer = new Uint8Array(new ArrayBuffer(24), 0, 0);
      buffer = VarInt.from(17).encode(buffer);
      buffer = frame.id.value.encode(buffer);
      buffer = VarInt.from(frame.max).encode(buffer);
      return buffer;
    }
    case "max_streams_bidi": {
      let buffer = new Uint8Array(new ArrayBuffer(16), 0, 0);
      buffer = VarInt.from(18).encode(buffer);
      buffer = VarInt.from(frame.max).encode(buffer);
      return buffer;
    }
    case "max_streams_uni": {
      let buffer = new Uint8Array(new ArrayBuffer(16), 0, 0);
      buffer = VarInt.from(19).encode(buffer);
      buffer = VarInt.from(frame.max).encode(buffer);
      return buffer;
    }
    case "data_blocked": {
      let buffer = new Uint8Array(new ArrayBuffer(16), 0, 0);
      buffer = VarInt.from(20).encode(buffer);
      buffer = VarInt.from(frame.limit).encode(buffer);
      return buffer;
    }
    case "stream_data_blocked": {
      let buffer = new Uint8Array(new ArrayBuffer(24), 0, 0);
      buffer = VarInt.from(21).encode(buffer);
      buffer = frame.id.value.encode(buffer);
      buffer = VarInt.from(frame.limit).encode(buffer);
      return buffer;
    }
    case "streams_blocked_bidi": {
      let buffer = new Uint8Array(new ArrayBuffer(16), 0, 0);
      buffer = VarInt.from(22).encode(buffer);
      buffer = VarInt.from(frame.limit).encode(buffer);
      return buffer;
    }
    case "streams_blocked_uni": {
      let buffer = new Uint8Array(new ArrayBuffer(16), 0, 0);
      buffer = VarInt.from(23).encode(buffer);
      buffer = VarInt.from(frame.limit).encode(buffer);
      return buffer;
    }
    case "transport_parameters": {
      const payload = encodeTransportParams(frame.params);
      let buffer = new Uint8Array(new ArrayBuffer(8 + 8 + payload.byteLength), 0, 0);
      buffer = VarInt.from(0x3f5153300d0a0d0an).encode(buffer);
      buffer = VarInt.from(payload.byteLength).encode(buffer);
      buffer = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength + payload.byteLength);
      buffer.set(payload, buffer.byteLength - payload.byteLength);
      return buffer;
    }
    case "ping_request": {
      let buffer = new Uint8Array(new ArrayBuffer(16), 0, 0);
      buffer = VarInt.from(QX_PING_REQUEST).encode(buffer);
      buffer = VarInt.from(frame.sequence).encode(buffer);
      return buffer;
    }
    case "ping_response": {
      let buffer = new Uint8Array(new ArrayBuffer(16), 0, 0);
      buffer = VarInt.from(QX_PING_RESPONSE).encode(buffer);
      buffer = VarInt.from(frame.sequence).encode(buffer);
      return buffer;
    }
    case "datagram": {
      const lengthVi = VarInt.from(frame.data.length);
      let buffer = new Uint8Array(new ArrayBuffer(8 + 8 + frame.data.length), 0, 0);
      buffer = VarInt.from(49).encode(buffer);
      buffer = lengthVi.encode(buffer);
      buffer = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength + frame.data.length);
      buffer.set(frame.data, buffer.byteLength - frame.data.length);
      return buffer;
    }
  }
}
function encodeTransportParams(params) {
  let buffer = new Uint8Array(new ArrayBuffer(256), 0, 0);
  function writeParam(buf, id, value) {
    if (value === 0n)
      return buf;
    const valVi = VarInt.from(value);
    buf = VarInt.from(id).encode(buf);
    buf = VarInt.from(valVi.size()).encode(buf);
    buf = valVi.encode(buf);
    return buf;
  }
  buffer = writeParam(buffer, 1, params.maxIdleTimeout);
  buffer = writeParam(buffer, 4, params.initialMaxData);
  buffer = writeParam(buffer, 5, params.initialMaxStreamDataBidiLocal);
  buffer = writeParam(buffer, 6, params.initialMaxStreamDataBidiRemote);
  buffer = writeParam(buffer, 7, params.initialMaxStreamDataUni);
  buffer = writeParam(buffer, 8, params.initialMaxStreamsBidi);
  buffer = writeParam(buffer, 9, params.initialMaxStreamsUni);
  buffer = writeParam(buffer, 32, params.maxDatagramFrameSize);
  buffer = writeParam(buffer, MAX_RECORD_SIZE_ID, params.maxRecordSize);
  if (params.resetStreamAt) {
    buffer = VarInt.from(RESET_STREAM_AT_PARAM_ID).encode(buffer);
    buffer = VarInt.from(0).encode(buffer);
  }
  return buffer;
}
function decodeTransportParams(buffer) {
  const params = { ...DEFAULT_TRANSPORT_PARAMS };
  const seen = /* @__PURE__ */ new Set();
  let v;
  while (buffer.byteLength > 0) {
    [v, buffer] = VarInt.decode(buffer);
    const id = v.value;
    [v, buffer] = VarInt.decode(buffer);
    const len = Number(v.value);
    if (buffer.byteLength < len) {
      throw new Error("transport parameter truncated");
    }
    const paramData = buffer.slice(0, len);
    buffer = buffer.slice(len);
    if (RECOGNIZED_PARAM_IDS.has(id)) {
      if (seen.has(id)) {
        throw new Error(`duplicate transport parameter 0x${id.toString(16)}`);
      }
      seen.add(id);
    }
    if (id === APPLICATION_PROTOCOLS_ID) {
      throw new Error("unexpected application_protocols parameter over WebSocket");
    }
    if (id === RESET_STREAM_AT_PARAM_ID) {
      if (paramData.byteLength !== 0) {
        throw new Error("reset_stream_at transport parameter must be empty");
      }
      params.resetStreamAt = true;
      continue;
    }
    if (id === 0x00n || id === 0x02n || id === 0x03n || id >= 0x0an && id <= 0x10n) {
      throw new Error(`forbidden QUIC v1 transport parameter 0x${id.toString(16)}`);
    }
    if (!RECOGNIZED_PARAM_IDS.has(id)) {
      continue;
    }
    const [paramValueVarInt, remaining] = VarInt.decode(paramData);
    if (remaining.byteLength !== 0) {
      throw new Error(`transport parameter 0x${id.toString(16)} has trailing bytes`);
    }
    const paramValue = paramValueVarInt.value;
    switch (id) {
      case 0x01n:
        params.maxIdleTimeout = paramValue;
        break;
      case 0x04n:
        params.initialMaxData = paramValue;
        break;
      case 0x05n:
        params.initialMaxStreamDataBidiLocal = paramValue;
        break;
      case 0x06n:
        params.initialMaxStreamDataBidiRemote = paramValue;
        break;
      case 0x07n:
        params.initialMaxStreamDataUni = paramValue;
        break;
      case 0x08n:
        params.initialMaxStreamsBidi = paramValue;
        break;
      case 0x09n:
        params.initialMaxStreamsUni = paramValue;
        break;
      case 0x20n:
        params.maxDatagramFrameSize = paramValue;
        break;
      case MAX_RECORD_SIZE_ID:
        params.maxRecordSize = paramValue;
        break;
      default:
        throw new Error(`unhandled recognized transport parameter 0x${id.toString(16)}`);
    }
  }
  return params;
}
function decode5(buffer, version2 = "webtransport") {
  if (buffer.length === 0) {
    throw new Error("Invalid frame: empty buffer");
  }
  if (version2 === "webtransport") {
    return decodeWebTransport(buffer);
  }
  return decodeQMux(buffer);
}
function take(buffer, len) {
  if (buffer.byteLength < len) {
    throw new Error(`frame truncated: need ${len} bytes, have ${buffer.byteLength}`);
  }
  return [buffer.slice(0, len), buffer.slice(len)];
}
function decodeRecord(buffer) {
  const frames = [];
  while (buffer.byteLength > 0) {
    const result = decodeQMuxOne(buffer);
    if (result === null)
      break;
    const [frame, remaining] = result;
    if (frame !== null) {
      frames.push(frame);
    }
    buffer = remaining;
  }
  return frames;
}
function decodeWebTransport(buffer) {
  const frameType = buffer[0];
  buffer = buffer.slice(1);
  let v;
  if (frameType === 4) {
    [v, buffer] = VarInt.decode(buffer);
    const id = new Id(v);
    [v, buffer] = VarInt.decode(buffer);
    const code = v;
    return { type: "reset_stream", id, code, finalSize: 0n };
  }
  if (frameType === 5) {
    [v, buffer] = VarInt.decode(buffer);
    const id = new Id(v);
    [v, buffer] = VarInt.decode(buffer);
    const code = v;
    return { type: "stop_sending", id, code };
  }
  if (frameType === 29 || frameType === 28) {
    [v, buffer] = VarInt.decode(buffer);
    const code = v;
    const reason2 = new TextDecoder().decode(buffer);
    return frameType === 29 ? { type: "application_close", code, reason: reason2 } : { type: "connection_close", code, reason: reason2 };
  }
  if (frameType === 8 || frameType === 9) {
    [v, buffer] = VarInt.decode(buffer);
    const id = new Id(v);
    return {
      type: "stream",
      id,
      data: buffer,
      fin: frameType === 9
    };
  }
  throw new Error(`Invalid frame type: ${frameType}`);
}
function decodeQMux(buffer) {
  let v;
  [v, buffer] = VarInt.decode(buffer);
  const frameType = v.value;
  if (frameType === 0x00n) {
    return null;
  }
  if (frameType >= 0x08n && frameType <= 0x0fn) {
    const hasOff = (frameType & 0x04n) !== 0n;
    const hasLen = (frameType & 0x02n) !== 0n;
    const hasFin = (frameType & 0x01n) !== 0n;
    [v, buffer] = VarInt.decode(buffer);
    const id = new Id(v);
    let offset = 0n;
    if (hasOff) {
      [v, buffer] = VarInt.decode(buffer);
      offset = v.value;
    }
    let data;
    if (hasLen) {
      [v, buffer] = VarInt.decode(buffer);
      const len = Number(v.value);
      [data, buffer] = take(buffer, len);
    } else {
      data = buffer;
    }
    return { type: "stream", id, offset, data, fin: hasFin };
  }
  if (frameType === 0x04n) {
    [v, buffer] = VarInt.decode(buffer);
    const id = new Id(v);
    [v, buffer] = VarInt.decode(buffer);
    const code = v;
    [v, buffer] = VarInt.decode(buffer);
    const finalSize = v.value;
    return { type: "reset_stream", id, code, finalSize };
  }
  if (frameType === RESET_STREAM_AT) {
    [v, buffer] = VarInt.decode(buffer);
    const id = new Id(v);
    [v, buffer] = VarInt.decode(buffer);
    const code = v;
    [v, buffer] = VarInt.decode(buffer);
    const finalSize = v.value;
    [v, buffer] = VarInt.decode(buffer);
    const reliableSize = v.value;
    if (reliableSize > finalSize) {
      throw new Error("RESET_STREAM_AT reliable_size exceeds final_size");
    }
    return { type: "reset_stream", id, code, finalSize, reliableSize };
  }
  if (frameType === 0x05n) {
    [v, buffer] = VarInt.decode(buffer);
    const id = new Id(v);
    [v, buffer] = VarInt.decode(buffer);
    const code = v;
    return { type: "stop_sending", id, code };
  }
  if (frameType === 0x1cn || frameType === 0x1dn) {
    [v, buffer] = VarInt.decode(buffer);
    const code = v;
    if (frameType === 0x1cn) {
      [v, buffer] = VarInt.decode(buffer);
    }
    [v, buffer] = VarInt.decode(buffer);
    const reasonLen = Number(v.value);
    let reasonBytes;
    [reasonBytes, buffer] = take(buffer, reasonLen);
    const reason2 = new TextDecoder().decode(reasonBytes);
    return frameType === 0x1dn ? { type: "application_close", code, reason: reason2 } : { type: "connection_close", code, reason: reason2 };
  }
  if (frameType === 0x10n) {
    [v, buffer] = VarInt.decode(buffer);
    return { type: "max_data", max: v.value };
  }
  if (frameType === 0x11n) {
    [v, buffer] = VarInt.decode(buffer);
    const id = new Id(v);
    [v, buffer] = VarInt.decode(buffer);
    return { type: "max_stream_data", id, max: v.value };
  }
  if (frameType === 0x12n) {
    [v, buffer] = VarInt.decode(buffer);
    return { type: "max_streams_bidi", max: v.value };
  }
  if (frameType === 0x13n) {
    [v, buffer] = VarInt.decode(buffer);
    return { type: "max_streams_uni", max: v.value };
  }
  if (frameType === 0x14n) {
    [v, buffer] = VarInt.decode(buffer);
    return { type: "data_blocked", limit: v.value };
  }
  if (frameType === 0x15n) {
    [v, buffer] = VarInt.decode(buffer);
    const id = new Id(v);
    [v, buffer] = VarInt.decode(buffer);
    return { type: "stream_data_blocked", id, limit: v.value };
  }
  if (frameType === 0x16n) {
    [v, buffer] = VarInt.decode(buffer);
    return { type: "streams_blocked_bidi", limit: v.value };
  }
  if (frameType === 0x17n) {
    [v, buffer] = VarInt.decode(buffer);
    return { type: "streams_blocked_uni", limit: v.value };
  }
  if (frameType === 0x3f5153300d0a0d0an) {
    [v, buffer] = VarInt.decode(buffer);
    const len = Number(v.value);
    let payload;
    [payload, buffer] = take(buffer, len);
    const params = decodeTransportParams(payload);
    return { type: "transport_parameters", params };
  }
  if (frameType === QX_PING_REQUEST) {
    [v, buffer] = VarInt.decode(buffer);
    return { type: "ping_request", sequence: v.value };
  }
  if (frameType === QX_PING_RESPONSE) {
    [v, buffer] = VarInt.decode(buffer);
    return { type: "ping_response", sequence: v.value };
  }
  if (frameType === 0x30n) {
    return { type: "datagram", data: buffer, lengthPrefixed: false };
  }
  if (frameType === 0x31n) {
    [v, buffer] = VarInt.decode(buffer);
    const len = Number(v.value);
    let data;
    [data, buffer] = take(buffer, len);
    return { type: "datagram", data, lengthPrefixed: true };
  }
  throw new Error(`Invalid QMux frame type: 0x${frameType.toString(16)}`);
}
function decodeQMuxOne(buffer) {
  if (buffer.byteLength === 0)
    return null;
  let v;
  [v, buffer] = VarInt.decode(buffer);
  const frameType = v.value;
  if (frameType === 0x00n) {
    return [null, buffer];
  }
  if (frameType >= 0x08n && frameType <= 0x0fn) {
    const hasOff = (frameType & 0x04n) !== 0n;
    const hasLen = (frameType & 0x02n) !== 0n;
    const hasFin = (frameType & 0x01n) !== 0n;
    [v, buffer] = VarInt.decode(buffer);
    const id = new Id(v);
    let offset = 0n;
    if (hasOff) {
      [v, buffer] = VarInt.decode(buffer);
      offset = v.value;
    }
    let data;
    if (hasLen) {
      [v, buffer] = VarInt.decode(buffer);
      const len = Number(v.value);
      [data, buffer] = take(buffer, len);
    } else {
      data = buffer;
      buffer = buffer.slice(buffer.byteLength);
    }
    return [{ type: "stream", id, offset, data, fin: hasFin }, buffer];
  }
  if (frameType === 0x04n) {
    [v, buffer] = VarInt.decode(buffer);
    const id = new Id(v);
    [v, buffer] = VarInt.decode(buffer);
    const code = v;
    [v, buffer] = VarInt.decode(buffer);
    const finalSize = v.value;
    return [{ type: "reset_stream", id, code, finalSize }, buffer];
  }
  if (frameType === RESET_STREAM_AT) {
    [v, buffer] = VarInt.decode(buffer);
    const id = new Id(v);
    [v, buffer] = VarInt.decode(buffer);
    const code = v;
    [v, buffer] = VarInt.decode(buffer);
    const finalSize = v.value;
    [v, buffer] = VarInt.decode(buffer);
    const reliableSize = v.value;
    if (reliableSize > finalSize) {
      throw new Error("RESET_STREAM_AT reliable_size exceeds final_size");
    }
    return [{ type: "reset_stream", id, code, finalSize, reliableSize }, buffer];
  }
  if (frameType === 0x05n) {
    [v, buffer] = VarInt.decode(buffer);
    const id = new Id(v);
    [v, buffer] = VarInt.decode(buffer);
    const code = v;
    return [{ type: "stop_sending", id, code }, buffer];
  }
  if (frameType === 0x1cn || frameType === 0x1dn) {
    [v, buffer] = VarInt.decode(buffer);
    const code = v;
    if (frameType === 0x1cn) {
      [v, buffer] = VarInt.decode(buffer);
    }
    [v, buffer] = VarInt.decode(buffer);
    const reasonLen = Number(v.value);
    let reasonBytes;
    [reasonBytes, buffer] = take(buffer, reasonLen);
    const reason2 = new TextDecoder().decode(reasonBytes);
    const close = frameType === 0x1dn ? { type: "application_close", code, reason: reason2 } : { type: "connection_close", code, reason: reason2 };
    return [close, buffer];
  }
  if (frameType === 0x10n) {
    [v, buffer] = VarInt.decode(buffer);
    return [{ type: "max_data", max: v.value }, buffer];
  }
  if (frameType === 0x11n) {
    [v, buffer] = VarInt.decode(buffer);
    const id = new Id(v);
    [v, buffer] = VarInt.decode(buffer);
    return [{ type: "max_stream_data", id, max: v.value }, buffer];
  }
  if (frameType === 0x12n) {
    [v, buffer] = VarInt.decode(buffer);
    return [{ type: "max_streams_bidi", max: v.value }, buffer];
  }
  if (frameType === 0x13n) {
    [v, buffer] = VarInt.decode(buffer);
    return [{ type: "max_streams_uni", max: v.value }, buffer];
  }
  if (frameType === 0x14n) {
    [v, buffer] = VarInt.decode(buffer);
    return [{ type: "data_blocked", limit: v.value }, buffer];
  }
  if (frameType === 0x15n) {
    [v, buffer] = VarInt.decode(buffer);
    const id = new Id(v);
    [v, buffer] = VarInt.decode(buffer);
    return [{ type: "stream_data_blocked", id, limit: v.value }, buffer];
  }
  if (frameType === 0x16n) {
    [v, buffer] = VarInt.decode(buffer);
    return [{ type: "streams_blocked_bidi", limit: v.value }, buffer];
  }
  if (frameType === 0x17n) {
    [v, buffer] = VarInt.decode(buffer);
    return [{ type: "streams_blocked_uni", limit: v.value }, buffer];
  }
  if (frameType === 0x3f5153300d0a0d0an) {
    [v, buffer] = VarInt.decode(buffer);
    const len = Number(v.value);
    let payload;
    [payload, buffer] = take(buffer, len);
    const params = decodeTransportParams(payload);
    return [{ type: "transport_parameters", params }, buffer];
  }
  if (frameType === QX_PING_REQUEST) {
    [v, buffer] = VarInt.decode(buffer);
    return [{ type: "ping_request", sequence: v.value }, buffer];
  }
  if (frameType === QX_PING_RESPONSE) {
    [v, buffer] = VarInt.decode(buffer);
    return [{ type: "ping_response", sequence: v.value }, buffer];
  }
  if (frameType === 0x30n) {
    const data = buffer;
    return [{ type: "datagram", data, lengthPrefixed: false }, buffer.slice(buffer.byteLength)];
  }
  if (frameType === 0x31n) {
    [v, buffer] = VarInt.decode(buffer);
    const len = Number(v.value);
    let data;
    [data, buffer] = take(buffer, len);
    return [{ type: "datagram", data, lengthPrefixed: true }, buffer];
  }
  throw new Error(`Invalid QMux frame type: 0x${frameType.toString(16)}`);
}

// scripts/moq-browser/node_modules/@moq/qmux/recv.js
var RecvStream = class {
  #queue = [];
  #fin = false;
  #error;
  #wake;
  #terminal = false;
  #onTerminal;
  /** The application-facing readable. */
  readable;
  /**
   * @param onConsume Invoked with each chunk's byte length as it is delivered
   *   to the reader. Drives MAX_STREAM_DATA.
   * @param onCancel Invoked with the number of discarded buffered bytes, and the reason the
   *   application passed to `cancel()`, when the application cancels the readable
   *   (→ STOP_SENDING, whose code comes from that reason).
   * @param onTerminal Invoked once when FIN, RESET_STREAM, or local cancellation
   *   makes the receive side terminal.
   */
  constructor(onConsume, onCancel, onTerminal = () => {
  }) {
    this.#onTerminal = onTerminal;
    this.readable = new ReadableStream({
      pull: async (controller) => {
        while (this.#queue.length === 0) {
          if (this.#error) {
            controller.error(this.#error);
            return;
          }
          if (this.#fin) {
            controller.close();
            return;
          }
          await new Promise((resolve2) => {
            this.#wake = resolve2;
          });
        }
        const chunk = this.#queue.shift();
        controller.enqueue(chunk);
        onConsume(chunk.byteLength);
      },
      cancel: (reason2) => {
        this.#error ??= new Error("stream cancelled");
        onCancel(this.#discard(), reason2);
        this.#notifyTerminal();
        this.#signal();
      }
    }, { highWaterMark: 0 });
  }
  /** Buffer a received chunk for delivery. Ignored after FIN/error. */
  push(chunk) {
    if (this.#fin || this.#error)
      return false;
    this.#queue.push(chunk);
    this.#signal();
    return true;
  }
  /** Mark end-of-stream; the readable closes once buffered data drains. */
  finish() {
    this.#fin = true;
    this.#notifyTerminal();
    this.#signal();
  }
  /** Abort the readable because the peer reset its sending side. */
  reset(err) {
    const discarded = this.error(err);
    this.#notifyTerminal();
    return discarded;
  }
  /** Abort the readable, discarding undelivered buffered data. */
  error(err) {
    if (this.#error)
      return 0;
    this.#error = err;
    const discarded = this.#discard();
    this.#signal();
    return discarded;
  }
  #discard() {
    let bytes = 0;
    for (const chunk of this.#queue)
      bytes += chunk.byteLength;
    this.#queue = [];
    return bytes;
  }
  #notifyTerminal() {
    if (this.#terminal)
      return;
    this.#terminal = true;
    this.#onTerminal();
  }
  #signal() {
    const wake = this.#wake;
    if (wake) {
      this.#wake = void 0;
      wake();
    }
  }
};

// scripts/moq-browser/node_modules/@moq/qmux/scheduler.js
var WritableStreamSink = class {
  #writer;
  constructor(writable) {
    this.#writer = writable.getWriter();
  }
  ready() {
    return this.#writer.ready;
  }
  write(bytes) {
    return this.#writer.write(bytes);
  }
  wantsMore() {
    const desired = this.#writer.desiredSize;
    return desired !== null && desired > 0;
  }
};
var DEFAULT_SEND_ORDER = 0;
var DATAGRAM_QUEUE_LIMIT = 1024;
var SendScheduler = class {
  #sink;
  #onActivity;
  #controlHighWater;
  #closed;
  // Control frames preempt all stream data, FIFO among themselves.
  #control = [];
  #controlBytes = 0;
  // Datagrams: serviced after control, ahead of stream data. Bounded and lossy
  // (unlike #control) since unreliable datagrams are meant to be droppable —
  // shed on transport backpressure or a full lane.
  #datagrams = [];
  // Per-stream send priority and the single pending frame per ready stream.
  // Invariant: a stream has at most one Waiter at a time, because its writer
  // task awaits each enqueue before producing the next frame. This keeps
  // per-stream byte order (offsets) sequential.
  #sendOrders = /* @__PURE__ */ new Map();
  #ready = /* @__PURE__ */ new Map();
  #seq = 0;
  // Resolver for the loop when it's parked with no work.
  #wake;
  constructor(sink, options) {
    this.#sink = sink;
    this.#onActivity = options?.onActivity ?? (() => {
    });
    this.#controlHighWater = options?.controlHighWater ?? 256 * 1024;
    void this.#run();
  }
  /** Queue a pre-encoded control frame. Preempts all stream data. */
  enqueueControl(bytes) {
    if (this.#closed)
      throw this.#closed;
    this.#control.push(bytes);
    this.#controlBytes += bytes.byteLength;
    if (this.#controlBytes > this.#controlHighWater) {
      console.warn(`qmux: control backlog ${this.#controlBytes} bytes exceeds high-water`);
    }
    this.#signal();
  }
  /** Queue a datagram frame — best-effort, serviced after control but ahead of
   *  stream data. Dropped (rather than queued) when:
   *   - the scheduler is closed;
   *   - the transport is backpressured — the socket is already queuing stream/
   *     control data, so a datagram behind it would arrive stale; or
   *   - the datagram lane is full, bounding a synchronous burst that outruns the
   *     writer loop even while the transport has room.
   *  An unreliable datagram is meant to be droppable, so this sheds rather than
   *  applying backpressure to the caller. */
  enqueueDatagram(bytes) {
    if (this.#closed)
      return;
    if (!this.#sink.wantsMore())
      return;
    if (this.#datagrams.length >= DATAGRAM_QUEUE_LIMIT)
      return;
    this.#datagrams.push(bytes);
    this.#signal();
  }
  /** Set (or update) a stream's send priority. Takes effect immediately,
   *  including for the stream's already-queued frame (priority is read at
   *  selection time), so promoting a stream lets it jump a lower-priority
   *  backlog without reordering its own bytes. */
  setSendOrder(streamId, order) {
    this.#sendOrders.set(streamId, order);
  }
  /** Queue a stream-data frame. Resolves once the bytes hit the socket;
   *  rejects if the session closes or the stream is dropped first. */
  enqueueStream(streamId, bytes) {
    if (this.#closed)
      return Promise.reject(this.#closed);
    if (this.#ready.has(streamId)) {
      return Promise.reject(new Error(`stream ${streamId} already has a queued frame`));
    }
    return new Promise((resolve2, reject) => {
      this.#ready.set(streamId, { seq: this.#seq++, bytes, resolve: resolve2, reject });
      this.#signal();
    });
  }
  /** Drop a stream's pending data (reset / abort). Rejects its in-flight frame. */
  dropStream(streamId, err) {
    this.#sendOrders.delete(streamId);
    const waiter = this.#ready.get(streamId);
    if (waiter) {
      this.#ready.delete(streamId);
      waiter.reject(err);
    }
  }
  /** Forget a stream that finished cleanly (its FIN is already queued/sent).
   *  Frees the per-stream priority entry so it doesn't accumulate. */
  forget(streamId) {
    this.#sendOrders.delete(streamId);
  }
  /** Close the scheduler: reject all pending stream frames, but let the loop
   *  flush any already-queued control frames (e.g. CONNECTION_CLOSE). */
  close(err) {
    if (this.#closed)
      return;
    this.#closed = err;
    for (const waiter of this.#ready.values())
      waiter.reject(err);
    this.#ready.clear();
    this.#sendOrders.clear();
    this.#datagrams.length = 0;
    this.#signal();
  }
  #signal() {
    const wake = this.#wake;
    if (wake) {
      this.#wake = void 0;
      wake();
    }
  }
  /** Pick the next stream to service: highest sendOrder, oldest seq to break
   *  ties (round-robin, since a stream re-arms with a fresh seq each frame). */
  #pickStream() {
    let bestId;
    let bestOrder = Number.NEGATIVE_INFINITY;
    let bestSeq = Number.POSITIVE_INFINITY;
    for (const [id, waiter] of this.#ready) {
      const order = this.#sendOrders.get(id) ?? DEFAULT_SEND_ORDER;
      if (order > bestOrder || order === bestOrder && waiter.seq < bestSeq) {
        bestOrder = order;
        bestSeq = waiter.seq;
        bestId = id;
      }
    }
    return bestId;
  }
  // biome-ignore lint/correctness/noUnusedPrivateClassMembers: invoked from the constructor; Biome's analysis misses the call into this infinite writer loop.
  async #run() {
    try {
      while (true) {
        if (this.#control.length === 0 && this.#datagrams.length === 0 && this.#ready.size === 0) {
          if (this.#closed)
            return;
          await new Promise((resolve2) => {
            this.#wake = resolve2;
          });
          continue;
        }
        await this.#sink.ready();
        if (this.#control.length > 0) {
          const bytes = this.#control.shift();
          this.#controlBytes -= bytes.byteLength;
          await this.#sink.write(bytes);
          this.#onActivity();
          continue;
        }
        if (this.#datagrams.length > 0) {
          const bytes = this.#datagrams.shift();
          await this.#sink.write(bytes);
          this.#onActivity();
          continue;
        }
        if (this.#closed)
          return;
        if (this.#ready.size === 0)
          continue;
        const id = this.#pickStream();
        const waiter = this.#ready.get(id);
        this.#ready.delete(id);
        try {
          await this.#sink.write(waiter.bytes);
        } catch (err) {
          waiter.reject(err instanceof Error ? err : new Error(String(err)));
          throw err;
        }
        this.#onActivity();
        waiter.resolve();
      }
    } catch (err) {
      this.#fail(err instanceof Error ? err : new Error(String(err)));
    }
  }
  #fail(err) {
    this.#closed ??= err;
    for (const waiter of this.#ready.values())
      waiter.reject(err);
    this.#ready.clear();
    this.#control.length = 0;
    this.#controlBytes = 0;
    this.#datagrams.length = 0;
  }
};

// scripts/moq-browser/node_modules/@moq/qmux/session.js
var DEFAULT_CONFIG = {
  maxStreamsBidi: 100n,
  maxStreamsUni: 100n,
  maxData: 1048576n,
  maxStreamDataBidiLocal: 262144n,
  maxStreamDataBidiRemote: 262144n,
  maxStreamDataUni: 262144n,
  maxIdleTimeout: 30000n,
  maxRecordSize: DEFAULT_MAX_RECORD_SIZE,
  // Fill a full record by default; the record layer bounds the size.
  maxDatagramFrameSize: DEFAULT_MAX_RECORD_SIZE
};
function configToTransportParams(config2) {
  return {
    maxIdleTimeout: config2.maxIdleTimeout,
    initialMaxData: config2.maxData,
    initialMaxStreamDataBidiLocal: config2.maxStreamDataBidiLocal,
    initialMaxStreamDataBidiRemote: config2.maxStreamDataBidiRemote,
    initialMaxStreamDataUni: config2.maxStreamDataUni,
    initialMaxStreamsBidi: config2.maxStreamsBidi,
    initialMaxStreamsUni: config2.maxStreamsUni,
    // Clamp to maxRecordSize so we never advertise a datagram larger than our
    // record layer accepts.
    maxDatagramFrameSize: config2.maxDatagramFrameSize < config2.maxRecordSize ? config2.maxDatagramFrameSize : config2.maxRecordSize,
    maxRecordSize: config2.maxRecordSize,
    // Version-gated in #startSession (only advertised on qmux-02).
    resetStreamAt: false
  };
}
var Datagrams = class {
  send;
  incomingHighWaterMark = 1024;
  incomingMaxAge = null;
  outgoingHighWaterMark = 1024;
  outgoingMaxAge = null;
  readable;
  writable;
  #incoming;
  // Resolved from the peer's transport parameters once the handshake completes;
  // 0 until then (and forever if the peer doesn't accept datagrams).
  #maxDatagramSize = 0;
  /** @param send Enqueue a datagram payload onto the wire (best-effort). */
  constructor(send) {
    this.send = send;
    this.readable = new ReadableStream({
      start: (controller) => {
        this.#incoming = controller;
      }
    }, { highWaterMark: this.incomingHighWaterMark });
    this.writable = new WritableStream({
      write: (chunk) => {
        if (this.#maxDatagramSize > 0 && chunk.byteLength <= this.#maxDatagramSize) {
          this.send(chunk);
        }
      }
    });
  }
  get maxDatagramSize() {
    return this.#maxDatagramSize;
  }
  /** Resolve the send-payload limit from the negotiated parameters. */
  setMaxDatagramSize(size2) {
    this.#maxDatagramSize = size2;
  }
  /** Deliver an inbound datagram to the reader, dropping it if the queue is full. */
  push(data) {
    if (this.#incoming.desiredSize === null || this.#incoming.desiredSize <= 0) {
      return;
    }
    this.#incoming.enqueue(data);
  }
  /** Close the inbound readable when the session ends. */
  close(err) {
    try {
      if (err)
        this.#incoming.error(err);
      else
        this.#incoming.close();
    } catch {
    }
  }
};
var IncomingStreamQueue = class {
  readable;
  #controller;
  #pending = [];
  #pullResolve;
  #cancelled = false;
  #closed = false;
  constructor() {
    this.readable = new ReadableStream({
      start: (controller) => {
        this.#controller = controller;
      },
      pull: () => {
        if (this.#deliver())
          return;
        return new Promise((resolve2) => {
          this.#pullResolve = resolve2;
        });
      },
      cancel: () => {
        this.#cancelled = true;
        this.#dropPending(new Error("incoming stream acceptor cancelled"));
        this.#resolvePull();
      }
    }, { highWaterMark: 0 });
  }
  push(value, onAccept, onDrop) {
    if (this.#cancelled || this.#closed)
      return false;
    this.#pending.push({ value, onAccept, onDrop });
    if (this.#pullResolve) {
      this.#deliver();
      this.#resolvePull();
    }
    return true;
  }
  close(err) {
    if (this.#closed || this.#cancelled)
      return;
    this.#closed = true;
    this.#dropPending(err ?? new Error("session closed"));
    try {
      if (err)
        this.#controller.error(err);
      else
        this.#controller.close();
    } catch {
    }
    this.#resolvePull();
  }
  #deliver() {
    const item = this.#pending.shift();
    if (!item)
      return false;
    try {
      this.#controller.enqueue(item.value);
      item.onAccept();
    } catch {
      item.onDrop(new Error("incoming stream acceptor cancelled"));
    }
    return true;
  }
  #dropPending(err) {
    const pending = this.#pending;
    this.#pending = [];
    for (const item of pending)
      item.onDrop(err);
  }
  #resolvePull() {
    const resolve2 = this.#pullResolve;
    this.#pullResolve = void 0;
    resolve2?.();
  }
};
var DEFAULT_SEND_BUFFER_SIZE = 64 * 1024;
function toWebSocketStream(socket, highWaterMark) {
  return "opened" in socket ? socket : WebSocketStream.adopt(socket, { highWaterMark });
}
function versionPrefix(version2) {
  switch (version2) {
    case "qmux-02":
      return "qmux-02.";
    case "qmux-01":
      return "qmux-01.";
    case "qmux-00":
      return "qmux-00.";
  }
}
var QMUX_VERSIONS = ["qmux-02", "qmux-01", "qmux-00"];
function toWebSocketUrl(url) {
  const u = typeof url === "string" ? new URL(url) : url;
  let scheme;
  switch (u.protocol) {
    case "https:":
    case "wss:":
      scheme = "wss:";
      break;
    case "http:":
    case "ws:":
      scheme = "ws:";
      break;
    default:
      throw new Error(`Unsupported protocol: ${u.protocol}`);
  }
  return `${scheme}//${u.host}${u.pathname}${u.search}`;
}
var BARE_ALPNS = ["qmux-02", "qmux-01", "qmux-00", "webtransport"];
function resolveSubprotocols(protocols, versions, requireProtocol) {
  const out = [];
  for (const entry of protocols) {
    const known = QMUX_VERSIONS.find((v) => entry.startsWith(versionPrefix(v)));
    if (known !== void 0) {
      out.push(entry);
      continue;
    }
    if (!(entry in versions)) {
      throw new Error(`Sec-WebSocket-Protocol entry ${JSON.stringify(entry)} has no qmux prefix and no versions mapping`);
    }
    const value = versions[entry];
    const expanded = value === null ? QMUX_VERSIONS : Array.isArray(value) ? value : [value];
    for (const v of expanded) {
      out.push(`${versionPrefix(v)}${entry}`);
    }
  }
  if (!requireProtocol) {
    out.push(...BARE_ALPNS);
  }
  return out;
}
function detectVersion(negotiated2) {
  for (const v of QMUX_VERSIONS) {
    if (negotiated2 === v || negotiated2.startsWith(versionPrefix(v))) {
      return v;
    }
  }
  return "webtransport";
}
function parseProtocol(raw, version2) {
  if (raw === "" || version2 === "webtransport")
    return "";
  if (raw === version2)
    return "";
  const prefix = versionPrefix(version2);
  return raw.startsWith(prefix) ? raw.slice(prefix.length) : "";
}
var RecvOpen = class {
  #createdMax;
  #holes = /* @__PURE__ */ new Set();
  isClosed(index) {
    return this.#createdMax !== void 0 && index <= this.#createdMax && !this.#holes.has(index);
  }
  record(index) {
    if (this.#createdMax !== void 0 && index <= this.#createdMax) {
      this.#holes.delete(index);
      return;
    }
    for (let hole = (this.#createdMax ?? -1n) + 1n; hole < index; hole++) {
      this.#holes.add(hole);
    }
    this.#createdMax = index;
  }
};
var Session = class _Session {
  // The transport: a native `WebSocketStream` when the platform has one (real
  // backpressure), otherwise the `@moq/web-socket-stream` ponyfill over a plain
  // `WebSocket` (bufferedAmount-based backpressure). Either way, one API.
  #wss;
  #scheduler;
  #sendBufferSize;
  // The stream-id role: false dials (client), true accepts (server). Set by
  // [[Session.accept]]; every id we mint and every id we allow the peer to mint
  // keys off it.
  #isServer = false;
  // [[AcceptOptions.protocol]]: stands in for `socket.protocol` when the host
  // doesn't expose the negotiated subprotocol.
  #protocolOverride;
  #closed;
  #closeReason;
  #sendStreams = /* @__PURE__ */ new Map();
  #recvStreams = /* @__PURE__ */ new Map();
  #nextUniStreamId = 0n;
  #nextBiStreamId = 0n;
  // Default to the legacy wire format until the WebSocket opens and the
  // negotiated subprotocol tells us otherwise. #handleOpen overrides this
  // with the actual version derived from `ws.protocol`.
  #version = "webtransport";
  /** The negotiated application-level subprotocol, or empty string if none.
   *
   * The prefix is stripped; this returns only the application protocol name.
   */
  #protocol = "";
  get protocol() {
    return this.#protocol;
  }
  ready;
  #readyResolve;
  #readyReject;
  closed;
  #closedResolve;
  #closedReject;
  incomingBidirectionalStreams;
  #incomingBidirectionalQueue;
  incomingUnidirectionalStreams;
  #incomingUnidirectionalQueue;
  datagrams = new Datagrams((data) => this.#sendDatagram(data));
  // Flow control state
  #config;
  #ourParams;
  #peerParams = { ...DEFAULT_TRANSPORT_PARAMS };
  #paramsReceived = false;
  // Send credits start at the legacy wire format's "unlimited" values to
  // match the default #version. #handleOpen replaces them with QMux-shaped
  // zero-credits (waiting for TRANSPORT_PARAMETERS) when the negotiated
  // version turns out to be a QMux draft.
  #connCredit = new Credit(BigInt(Number.MAX_SAFE_INTEGER));
  // Connection-level recv flow control
  #recvDataOffset = 0n;
  #recvDataMax = 0n;
  #recvDataConsumed = 0n;
  // Per-stream flow control
  #streamFlow = /* @__PURE__ */ new Map();
  #recvOpenBi = new RecvOpen();
  #recvOpenUni = new RecvOpen();
  // Stream count tracking via Credit (for sending — peer's limits).
  // Initialized to "unlimited" matching the default webtransport version;
  // #handleOpen replaces them when a QMux draft is negotiated.
  #bidiStreamCredit = new Credit(BigInt(Number.MAX_SAFE_INTEGER));
  #uniStreamCredit = new Credit(BigInt(Number.MAX_SAFE_INTEGER));
  // Stream count tracking via Credit (for receiving — our limits)
  #recvBiCredit;
  #recvUniCredit;
  // QMux01 idle-timeout tracking (engaged once we've received the peer's params).
  #lastRecvAt = Date.now();
  #lastSendAt = Date.now();
  // Deadline bookkeeping for #idleActivityAt: the newest receive observed, the
  // send last allowed to restart the deadline, and whether a send may restart it.
  #recvSeen = 0;
  #sendResetAt = 0;
  #sendCredit = true;
  #nextPingSeq = 0;
  // Highest sequence seen in a received QX_PING request (draft-02 requires them
  // to strictly increase). Undefined until the first request arrives.
  #lastPingRecv;
  #idleTimer;
  /** Open a QMux session as the **client**, dialing `url`.
   *
   * The polyfill constructs the underlying `WebSocket` itself. Pass the
   * application-level ALPNs in `options.protocols` plus a `versions`
   * map saying which QMux wire-format version each bare ALPN rides on. The
   * wire form `{qmux-VV}.{alpn}` is built automatically; entries already in
   * pair form (e.g. `"qmux-00.moq-transport-17"`) pass through unchanged.
   *
   * Once the handshake completes, the QMux wire-format version is derived
   * from the negotiated `Sec-WebSocket-Protocol`. `.protocol` exposes the
   * application protocol with the QMux prefix stripped.
   *
   * Pass an already-connected `WebSocket` (or `WebSocketStream`) instead of a
   * URL to run the client role over a socket you opened yourself — the
   * subprotocol was chosen when you created it, so `options.protocols` and
   * `options.versions` are ignored. See [[Session.accept]] for the server role.
   */
  constructor(source, options) {
    if (options?.requireUnreliable) {
      throw new Error("not allowed to use WebSocket; requireUnreliable is true");
    }
    if (options?.serverCertificateHashes) {
      console.warn("serverCertificateHashes is not supported; trying anyway");
    }
    const dial = typeof source === "string" || source instanceof URL;
    const subprotocols = dial ? resolveSubprotocols(options?.protocols ?? [], options?.versions ?? {}, options?.requireProtocol ?? false) : [];
    this.#config = { ...DEFAULT_CONFIG, ...options?.config };
    this.#ourParams = configToTransportParams(this.#config);
    this.#sendBufferSize = options?.sendBufferSize ?? DEFAULT_SEND_BUFFER_SIZE;
    this.#recvBiCredit = new Credit(this.#config.maxStreamsBidi);
    this.#recvUniCredit = new Credit(this.#config.maxStreamsUni);
    const ready = Promise.withResolvers();
    this.ready = ready.promise;
    this.#readyResolve = ready.resolve;
    this.#readyReject = ready.reject;
    this.ready.catch(() => {
    });
    const closed = Promise.withResolvers();
    this.closed = closed.promise;
    this.#closedResolve = closed.resolve;
    this.#closedReject = closed.reject;
    this.closed.catch(() => {
    });
    this.#incomingBidirectionalQueue = new IncomingStreamQueue();
    this.incomingBidirectionalStreams = this.#incomingBidirectionalQueue.readable;
    this.#incomingUnidirectionalQueue = new IncomingStreamQueue();
    this.incomingUnidirectionalStreams = this.#incomingUnidirectionalQueue.readable;
    this.#attach(dial ? (
      // Open the transport via `WebSocketStream` — native when present (real
      // backpressure), else the ponyfill over a plain `WebSocket`. One code
      // path for both, so the path exercised in tests is the one Chromium
      // runs natively.
      openWebSocketStream(toWebSocketUrl(source), {
        protocols: subprotocols,
        highWaterMark: this.#sendBufferSize
      })
    ) : toWebSocketStream(source, this.#sendBufferSize));
  }
  /** Open a QMux session as the **server**, over a socket you have already
   *  accepted from an HTTP upgrade.
   *
   * The host performs the WebSocket handshake (`Deno.upgradeWebSocket`, `ws`,
   * ...), so by the time you get here the subprotocol is already negotiated:
   * the wire-format version and `.protocol` are read off the socket rather than
   * advertised. Use [[selectSubprotocol]] to pick that value during the upgrade.
   *
   * The returned session is a `WebTransport`, identical to a client session
   * apart from the stream-id role — so it accepts peer-initiated streams and
   * opens server-initiated ones.
   *
   * @example
   * ```ts
   * const { socket, response } = Deno.upgradeWebSocket(req, { protocol });
   * const session = Session.accept(socket, { config });
   * await session.ready;
   * ```
   */
  static accept(socket, options) {
    const session = new _Session(socket, options);
    session.#isServer = true;
    session.#protocolOverride = options?.protocol;
    return session;
  }
  /** Drive the session off an opened transport, whichever way we got it. */
  #attach(wss) {
    this.#wss = wss;
    wss.opened.then((conn) => {
      if (this.#closed)
        return;
      this.#startSession(this.#protocolOverride ?? conn.protocol, new WritableStreamSink(conn.writable));
      void this.#readLoop(conn.readable.getReader());
    }, (err) => {
      this.#closeReason ??= err instanceof Error ? err : new Error("WebSocketStream failed to open");
      this.#abort(1006, "WebSocketStream error");
    });
    wss.closed.then((info) => {
      this.#closeReason ??= new Error(`Connection closed: ${info.closeCode ?? 0} ${info.reason ?? ""}`);
      this.#abort(info.closeCode ?? 1006, info.reason ?? "");
    }, (err) => {
      this.#closeReason ??= err instanceof Error ? err : new Error("WebSocketStream closed");
      this.#abort(1006, "WebSocketStream error");
    });
  }
  async #readLoop(reader) {
    try {
      while (true) {
        const { value, done } = await reader.read();
        if (done)
          break;
        if (typeof value === "string") {
          this.#sendConnectionClose(1003, "text frames are not valid for QMux");
          this.#abort(1003, "text frames are not valid for QMux");
          return;
        }
        this.#onData(value instanceof ArrayBuffer ? new Uint8Array(value) : value);
      }
    } catch (err) {
      this.#closeReason ??= err instanceof Error ? err : new Error("WebSocketStream read error");
      this.#abort(1006, "WebSocketStream read error");
    }
  }
  /** Derive the wire-format version, start the send scheduler, and (for QMux
   *  drafts) exchange transport parameters. Shared by both transports. */
  #startSession(rawProtocol, sink) {
    const version2 = detectVersion(rawProtocol);
    this.#version = version2;
    this.#protocol = parseProtocol(rawProtocol, version2);
    this.#ourParams = {
      ...this.#ourParams,
      maxDatagramFrameSize: usesRecords(version2) ? this.#ourParams.maxDatagramFrameSize : 0n,
      resetStreamAt: version2 === "qmux-02"
    };
    this.#scheduler = new SendScheduler(sink, {
      onActivity: () => {
        this.#lastSendAt = Date.now();
      }
    });
    if (isQmux(version2)) {
      this.#connCredit.close();
      this.#bidiStreamCredit.close();
      this.#uniStreamCredit.close();
      this.#connCredit = new Credit(0n);
      this.#bidiStreamCredit = new Credit(0n);
      this.#uniStreamCredit = new Credit(0n);
      this.#recvDataMax = this.#ourParams.initialMaxData;
      this.#sendTransportParameters();
    }
    this.#readyResolve();
  }
  #onData(data) {
    this.#lastRecvAt = Date.now();
    try {
      if (usesRecords(this.#version)) {
        if (BigInt(data.byteLength) > this.#ourParams.maxRecordSize) {
          throw new Error(`record exceeds our max_record_size (${data.byteLength} > ${this.#ourParams.maxRecordSize})`);
        }
        const frames = decodeRecord(data);
        for (const frame of frames) {
          this.#recvFrame(frame);
        }
      } else {
        if (this.#version === "qmux-00" && data.byteLength > MAX_FRAME_SIZE) {
          throw new Error(`frame exceeds max_frame_size (${data.byteLength} > ${MAX_FRAME_SIZE})`);
        }
        const frame = decode5(data, this.#version);
        if (frame !== null) {
          this.#recvFrame(frame);
        }
      }
    } catch (error2) {
      console.error("Protocol violation:", error2);
      this.#sendConnectionClose(1002, "Protocol violation");
      this.#abort(1002, "Protocol violation", error2);
    }
  }
  #recvFrame(frame) {
    if (this.#version === "qmux-02") {
      const isParams = frame.type === "transport_parameters";
      if (isParams === this.#paramsReceived) {
        throw new Error("QX_TRANSPORT_PARAMETERS must be the first frame");
      }
    }
    if (frame.type === "stream") {
      this.#handleStreamFrame(frame);
    } else if (frame.type === "reset_stream") {
      this.#handleResetStream(frame);
    } else if (frame.type === "stop_sending") {
      this.#handleStopSending(frame);
    } else if (frame.type === "application_close" || frame.type === "connection_close") {
      this.#closeReason ??= new Error(`Connection closed: ${frame.code.value} ${frame.reason}`);
      if (frame.type === "application_close") {
        this.#close(Number(frame.code.value), frame.reason);
      } else {
        this.#abort(Number(frame.code.value), frame.reason);
      }
      this.#transportClose();
    } else if (frame.type === "transport_parameters") {
      this.#handleTransportParameters(frame.params);
    } else if (frame.type === "max_data") {
      this.#connCredit.increaseMax(frame.max);
    } else if (frame.type === "max_stream_data") {
      const flow = this.#streamFlow.get(frame.id.value.value);
      if (flow)
        flow.sendCredit.increaseMax(frame.max);
    } else if (frame.type === "max_streams_bidi") {
      this.#bidiStreamCredit.increaseMax(frame.max);
    } else if (frame.type === "max_streams_uni") {
      this.#uniStreamCredit.increaseMax(frame.max);
    } else if (frame.type === "datagram") {
      if (this.#ourParams.maxDatagramFrameSize === 0n) {
        throw new Error("received a DATAGRAM but did not advertise datagram support");
      }
      const len = frame.data.byteLength;
      const header = frame.lengthPrefixed === false ? 1 : 1 + VarInt.from(len).size();
      const frameSize = BigInt(header + len);
      if (frameSize > this.#ourParams.maxDatagramFrameSize) {
        throw new Error("received a DATAGRAM larger than our advertised max_datagram_frame_size");
      }
      this.datagrams.push(frame.data);
    } else if (frame.type === "ping_request") {
      if (this.#version === "qmux-02") {
        if (this.#lastPingRecv !== void 0 && frame.sequence <= this.#lastPingRecv) {
          throw new Error("QX_PING request sequence must strictly increase");
        }
        this.#lastPingRecv = frame.sequence;
      }
      this.#sendPriorityFrame({ type: "ping_response", sequence: frame.sequence });
    } else if (frame.type === "ping_response") {
      if (this.#version === "qmux-02" && frame.sequence >= BigInt(this.#nextPingSeq)) {
        throw new Error("QX_PING response echoed a sequence we never sent");
      }
    } else if (frame.type === "data_blocked" || frame.type === "stream_data_blocked" || frame.type === "streams_blocked_bidi" || frame.type === "streams_blocked_uni") {
    }
  }
  #handleTransportParameters(params) {
    if (this.#paramsReceived)
      return;
    if (usesRecords(this.#version) && params.maxRecordSize < DEFAULT_MAX_RECORD_SIZE) {
      throw new Error("max_record_size below the default minimum");
    }
    this.#paramsReceived = true;
    this.#peerParams = params;
    this.#connCredit.increaseMax(params.initialMaxData);
    this.#bidiStreamCredit.increaseMax(params.initialMaxStreamsBidi);
    this.#uniStreamCredit.increaseMax(params.initialMaxStreamsUni);
    if (usesRecords(this.#version) && params.maxDatagramFrameSize > 0n) {
      const cap = params.maxRecordSize < params.maxDatagramFrameSize ? params.maxRecordSize : params.maxDatagramFrameSize;
      const overhead = BigInt(1 + VarInt.from(cap).size());
      const payload = cap > overhead ? cap - overhead : 0n;
      this.datagrams.setMaxDatagramSize(Number(payload));
    }
    for (const [streamIdVal, flow] of this.#streamFlow) {
      const id = new Id(VarInt.from(streamIdVal));
      const sendLimit = id.dir === Dir.Bi ? params.initialMaxStreamDataBidiRemote : params.initialMaxStreamDataUni;
      flow.sendCredit.increaseMax(sendLimit);
    }
    this.#startIdleTimerIfEnabled();
  }
  /** Effective idle timeout in ms, or 0 if disabled.
   *
   * Per RFC 9000 §10.1, the effective value is `min(our, peer)` of the non-zero advertised values
   * (or the single non-zero one). If both are zero, idle timeouts are disabled.
   */
  #effectiveIdleTimeoutMs() {
    if (!usesRecords(this.#version))
      return 0n;
    const a = this.#ourParams.maxIdleTimeout;
    const b = this.#peerParams.maxIdleTimeout;
    if (a === 0n && b === 0n)
      return 0n;
    if (a === 0n)
      return b;
    if (b === 0n)
      return a;
    return a < b ? a : b;
  }
  #startIdleTimerIfEnabled() {
    const timeoutMs = this.#effectiveIdleTimeoutMs();
    if (timeoutMs === 0n)
      return;
    const tickMs = Math.max(50, Number(timeoutMs) / 6);
    this.#idleTimer = setInterval(() => this.#idleTick(Number(timeoutMs)), tickMs);
  }
  /**
   * Newest activity that counts toward the idle deadline.
   *
   * A received frame always restarts the timer: it is the only direct proof the
   * peer is still there. A send restarts it too, but at most once per receive
   * (RFC 9000 §10.1). That proviso is what keeps the deadline reachable: our own
   * keep-alive pings advance `#lastSendAt`, so counting every send would let them
   * restart the very deadline they exist to test, and a peer that goes silent
   * while its socket still accepts our writes would never be reclaimed.
   *
   * Crediting the first send after each receive is what still lets a mostly
   * one-way sender stay open — its peer answers the keep-alive, and each answer
   * re-arms the credit.
   */
  #idleActivityAt() {
    if (this.#lastRecvAt > this.#recvSeen) {
      this.#recvSeen = this.#lastRecvAt;
      this.#sendCredit = true;
    }
    if (this.#sendCredit && this.#lastSendAt > this.#sendResetAt) {
      this.#sendResetAt = this.#lastSendAt;
      this.#sendCredit = false;
    }
    return Math.max(this.#recvSeen, this.#sendResetAt);
  }
  #idleTick(timeoutMs) {
    if (this.#closed) {
      if (this.#idleTimer)
        clearInterval(this.#idleTimer);
      return;
    }
    const now = Date.now();
    if (now - this.#idleActivityAt() > timeoutMs) {
      this.#closeReason ??= new Error("idle timeout");
      this.#abort(0, "idle timeout");
      this.#transportClose();
      return;
    }
    if (now - this.#lastSendAt > timeoutMs / 3) {
      const seq = this.#nextPingSeq;
      this.#nextPingSeq += 1;
      try {
        this.#sendPriorityFrame({ type: "ping_request", sequence: BigInt(seq) });
      } catch (e) {
        console.warn("qmux: keep-alive ping failed", e);
      }
    }
  }
  async #claimSendCredit(streamId, desired) {
    const flow = this.#streamFlow.get(streamId);
    if (!flow)
      return desired;
    while (true) {
      const streamClaimed = flow.sendCredit.tryClaim(desired);
      if (streamClaimed === 0n) {
        if (this.#closed)
          throw this.#closed;
        const claimed = await flow.sendCredit.claim(desired);
        flow.sendCredit.release(claimed);
        continue;
      }
      const connClaimed = this.#connCredit.tryClaim(streamClaimed);
      if (connClaimed === 0n) {
        flow.sendCredit.release(streamClaimed);
        if (this.#closed)
          throw this.#closed;
        const claimed = await this.#connCredit.claim(1n);
        this.#connCredit.release(claimed);
        continue;
      }
      if (connClaimed < streamClaimed) {
        flow.sendCredit.release(streamClaimed - connClaimed);
      }
      return connClaimed;
    }
  }
  #accountRecv(streamId, bytes) {
    if (!isQmux(this.#version) || bytes === 0)
      return true;
    const bytesN = BigInt(bytes);
    if (this.#recvDataOffset + bytesN > this.#recvDataMax) {
      return false;
    }
    this.#recvDataOffset += bytesN;
    const flow = this.#streamFlow.get(streamId);
    if (flow) {
      if (flow.recvOffset + bytesN > flow.recvMax) {
        return false;
      }
      flow.recvOffset += bytesN;
    }
    return true;
  }
  /** Connection-level credit. Accounted when bytes are delivered to the
   *  application or deliberately discarded, never merely on receipt. This keeps
   *  completed-but-unread streams inside the aggregate receive-memory window.
   *  `recvDataConsumed` is cumulative. */
  #accountConnConsumed(bytes) {
    if (!isQmux(this.#version) || bytes === 0 || bytes === 0n)
      return;
    this.#recvDataConsumed += BigInt(bytes);
    this.#maybeSendMaxData();
  }
  /** Stream-level credit. Accounted on *delivery* to the application (driven by
   *  RecvStream.onConsume), so MAX_STREAM_DATA tracks the read rate and the peer
   *  can't buffer more than one window ahead of a slow reader. `recvConsumed`
   *  is cumulative. */
  #accountStreamConsumed(streamId, bytes) {
    if (!isQmux(this.#version) || bytes === 0)
      return;
    const flow = this.#streamFlow.get(streamId);
    if (flow) {
      flow.recvConsumed += BigInt(bytes);
      this.#maybeSendMaxStreamData(streamId, flow);
    }
  }
  #maybeSendMaxData() {
    const newMax = replenishWindow(this.#recvDataConsumed, this.#recvDataMax, this.#ourParams.initialMaxData);
    if (newMax !== null) {
      this.#recvDataMax = newMax;
      this.#sendPriorityFrame({ type: "max_data", max: newMax });
    }
  }
  #maybeSendMaxStreamData(streamId, flow) {
    const id = new Id(VarInt.from(streamId));
    let initialWindow;
    if (id.dir === Dir.Bi) {
      initialWindow = id.serverInitiated === this.#isServer ? this.#ourParams.initialMaxStreamDataBidiLocal : this.#ourParams.initialMaxStreamDataBidiRemote;
    } else {
      initialWindow = this.#ourParams.initialMaxStreamDataUni;
    }
    const newMax = replenishWindow(flow.recvConsumed, flow.recvMax, initialWindow);
    if (newMax !== null) {
      flow.recvMax = newMax;
      this.#sendPriorityFrame({ type: "max_stream_data", id, max: newMax });
    }
  }
  /** Replenish stream count credit for a peer-initiated stream and send MAX_STREAMS if needed. */
  #replenishStreamCredit(dir) {
    if (!isQmux(this.#version))
      return;
    const credit = dir === Dir.Bi ? this.#recvBiCredit : this.#recvUniCredit;
    const newMax = credit.consume(1n);
    if (newMax !== null) {
      if (dir === Dir.Bi) {
        this.#sendPriorityFrame({ type: "max_streams_bidi", max: newMax });
      } else {
        this.#sendPriorityFrame({ type: "max_streams_uni", max: newMax });
      }
    }
  }
  #recvOpen(dir) {
    return dir === Dir.Bi ? this.#recvOpenBi : this.#recvOpenUni;
  }
  /** Account the previously-unseen tail declared by RESET_STREAM.
   *
   * Drafts through -02 were emitted by implementations that used an incorrect
   * zero final size, so tolerate a value below the bytes already received. The
   * stricter FINAL_SIZE_ERROR check starts with draft-03; meanwhile the larger
   * of the two values still prevents a reset from undoing flow-control usage. */
  #accountReset(frame) {
    if (!isQmux(this.#version))
      return 0n;
    const streamId = frame.id.value.value;
    const flow = this.#streamFlow.get(streamId);
    const received = flow?.recvOffset ?? 0n;
    const finalSize = frame.finalSize > received ? frame.finalSize : received;
    const gap = finalSize - received;
    const recvMax = flow?.recvMax ?? (frame.id.dir === Dir.Bi ? this.#ourParams.initialMaxStreamDataBidiRemote : this.#ourParams.initialMaxStreamDataUni);
    if (finalSize > recvMax || this.#recvDataOffset + gap > this.#recvDataMax) {
      return null;
    }
    this.#recvDataOffset += gap;
    if (flow)
      flow.recvOffset = finalSize;
    return gap;
  }
  /** Delete stream flow state only when both send and recv sides are gone. */
  #maybeDeleteStreamFlow(streamId) {
    if (!this.#sendStreams.has(streamId) && !this.#recvStreams.has(streamId)) {
      const flow = this.#streamFlow.get(streamId);
      if (flow) {
        flow.sendCredit.close();
        this.#streamFlow.delete(streamId);
      }
    }
  }
  #handleStreamFrame(frame) {
    if (this.#closed)
      return;
    const streamId = frame.id.value.value;
    if (!frame.id.canRecv(this.#isServer)) {
      throw new Error("Invalid stream ID direction");
    }
    let recv = this.#recvStreams.get(streamId);
    if (isQmux(this.#version) && frame.id.serverInitiated !== this.#isServer && !recv && this.#recvOpen(frame.id.dir).isClosed(frame.id.index)) {
      return;
    }
    if (!recv) {
      if (frame.id.serverInitiated === this.#isServer) {
        return;
      }
      if (!frame.id.canRecv(this.#isServer)) {
        throw new Error("received write-only stream");
      }
      if (isQmux(this.#version)) {
        const credit = frame.id.dir === Dir.Bi ? this.#recvBiCredit : this.#recvUniCredit;
        if (!credit.receiveUpTo(frame.id.index + 1n)) {
          this.#sendConnectionClose(1002, "stream limit exceeded");
          this.#abort(1002, "stream limit exceeded");
          return;
        }
        this.#recvOpen(frame.id.dir).record(frame.id.index);
      }
      if (isQmux(this.#version)) {
        const recvMax = frame.id.dir === Dir.Bi ? this.#ourParams.initialMaxStreamDataBidiRemote : this.#ourParams.initialMaxStreamDataUni;
        const sendMax = frame.id.dir === Dir.Bi ? this.#peerParams.initialMaxStreamDataBidiLocal : 0n;
        this.#streamFlow.set(streamId, {
          sendCredit: new Credit(sendMax),
          sendOffset: 0n,
          recvMax,
          recvOffset: 0n,
          recvConsumed: 0n
        });
      }
      if (!this.#accountRecv(streamId, frame.data.byteLength)) {
        this.#sendConnectionClose(1002, "flow control error");
        this.#abort(1002, "flow control error");
        return;
      }
      let accepted = false;
      let terminal = false;
      let streamCreditReplenished = false;
      const maybeReplenishStreamCredit = () => {
        if (!accepted || !terminal || streamCreditReplenished)
          return;
        streamCreditReplenished = true;
        this.#replenishStreamCredit(frame.id.dir);
      };
      const onAccept = () => {
        accepted = true;
        maybeReplenishStreamCredit();
      };
      const onTerminal = () => {
        terminal = true;
        maybeReplenishStreamCredit();
      };
      const recvStream = new RecvStream((bytes) => {
        this.#accountStreamConsumed(streamId, bytes);
        this.#accountConnConsumed(bytes);
      }, (discarded, reason2) => {
        this.#accountConnConsumed(discarded);
        this.#sendPriorityFrame({
          type: "stop_sending",
          id: frame.id,
          code: VarInt.from(resetCode(reason2))
        });
        this.#recvStreams.delete(streamId);
        this.#maybeDeleteStreamFlow(streamId);
      }, onTerminal);
      this.#recvStreams.set(streamId, recvStream);
      recv = recvStream;
      const reader = recvStream.readable;
      const onDrop = (err) => {
        this.#accountConnConsumed(recvStream.error(err));
      };
      if (frame.id.dir === Dir.Bi) {
        const writer = new WritableStream({
          start: (controller) => {
            this.#sendStreams.set(streamId, controller);
          },
          write: async (chunk) => {
            await this.#sendStreamData(frame.id, chunk);
          },
          abort: (e) => {
            this.#scheduler?.dropStream(streamId, e instanceof Error ? e : new Error("stream aborted"));
            this.#sendPriorityFrame({
              type: "reset_stream",
              id: frame.id,
              code: VarInt.from(resetCode(e)),
              finalSize: this.#sendFinalSize(streamId)
            });
            this.#sendStreams.delete(streamId);
            this.#maybeDeleteStreamFlow(streamId);
          },
          close: async () => {
            await this.#sendStreamFin(frame.id);
            this.#sendStreams.delete(streamId);
            this.#scheduler?.forget(streamId);
            this.#maybeDeleteStreamFlow(streamId);
          }
        });
        this.#attachSendOrder(writer, streamId, DEFAULT_SEND_ORDER);
        if (!this.#incomingBidirectionalQueue.push({ readable: reader, writable: writer }, onAccept, onDrop)) {
          onDrop(new Error("incoming stream acceptor cancelled"));
        }
      } else {
        if (!this.#incomingUnidirectionalQueue.push(reader, onAccept, onDrop)) {
          onDrop(new Error("incoming stream acceptor cancelled"));
        }
      }
    } else {
      if (!this.#accountRecv(streamId, frame.data.byteLength)) {
        this.#sendConnectionClose(1002, "flow control error");
        this.#abort(1002, "flow control error");
        return;
      }
    }
    if (frame.data.byteLength > 0) {
      if (!recv.push(frame.data))
        this.#accountConnConsumed(frame.data.byteLength);
    }
    if (frame.fin) {
      recv.finish();
      this.#recvStreams.delete(streamId);
      this.#maybeDeleteStreamFlow(streamId);
    }
  }
  #handleResetStream(frame) {
    if (frame.reliableSize !== void 0 && !this.#ourParams.resetStreamAt) {
      throw new Error("RESET_STREAM_AT received without advertising reset_stream_at");
    }
    if (!frame.id.canRecv(this.#isServer)) {
      throw new Error("Invalid stream ID direction");
    }
    const streamId = frame.id.value.value;
    const recv = this.#recvStreams.get(streamId);
    const peerInitiated = frame.id.serverInitiated !== this.#isServer;
    if (!recv) {
      if (isQmux(this.#version) && peerInitiated && this.#recvOpen(frame.id.dir).isClosed(frame.id.index)) {
        return;
      }
      if (!peerInitiated)
        return;
      if (isQmux(this.#version)) {
        const credit = frame.id.dir === Dir.Bi ? this.#recvBiCredit : this.#recvUniCredit;
        if (!credit.receiveUpTo(frame.id.index + 1n)) {
          this.#sendConnectionClose(1002, "stream limit exceeded");
          this.#abort(1002, "stream limit exceeded");
          return;
        }
      }
    }
    const resetGap = this.#accountReset(frame);
    if (resetGap === null) {
      this.#sendConnectionClose(1002, "flow control error");
      this.#abort(1002, "flow control error");
      return;
    }
    if (!recv) {
      this.#accountConnConsumed(resetGap);
      if (isQmux(this.#version))
        this.#recvOpen(frame.id.dir).record(frame.id.index);
      this.#replenishStreamCredit(frame.id.dir);
      return;
    }
    const discarded = recv.reset(new StreamError(streamCode2(frame.code.value), "RESET_STREAM"));
    this.#accountConnConsumed(resetGap + BigInt(discarded));
    this.#recvStreams.delete(streamId);
    this.#maybeDeleteStreamFlow(streamId);
  }
  #handleStopSending(frame) {
    const streamId = frame.id.value.value;
    const stream = this.#sendStreams.get(streamId);
    if (!stream)
      return;
    const stopped = new StreamError(streamCode2(frame.code.value), "STOP_SENDING");
    stream.error(stopped);
    this.#sendStreams.delete(streamId);
    this.#scheduler?.dropStream(streamId, stopped);
    this.#sendPriorityFrame({
      type: "reset_stream",
      id: frame.id,
      code: frame.code,
      finalSize: this.#sendFinalSize(streamId)
    });
    this.#maybeDeleteStreamFlow(streamId);
  }
  #sendFinalSize(streamId) {
    return this.#streamFlow.get(streamId)?.sendOffset ?? 0n;
  }
  #sendTransportParameters() {
    this.#sendPriorityFrame({ type: "transport_parameters", params: this.#ourParams });
  }
  /** The largest STREAM frame the peer accepts, in bytes.
   *
   * Record-framed drafts negotiate it: a frame rides in one record, so the
   * peer's `max_record_size` is the limit (the draft-01 default until its
   * TRANSPORT_PARAMETERS arrive, so we never send something it would reject).
   * draft-00 and the legacy binding have no record layer and negotiate nothing,
   * so draft-00's whole-frame `max_frame_size` stands in. */
  #sendFrameBudget() {
    if (!usesRecords(this.#version))
      return BigInt(MAX_FRAME_SIZE);
    return this.#paramsReceived ? this.#peerParams.maxRecordSize : DEFAULT_MAX_RECORD_SIZE;
  }
  /** The largest payload we may send in one STREAM frame at `offset`.
   *
   * This respects the peer's frame budget and the send-only compatibility
   * ceiling for released receivers. A `bigint`: the peer's `max_record_size` is
   * a varint, so it can exceed what `number` holds exactly. Callers clamp it to
   * the bytes they actually have before converting. */
  #maxStreamPayload(id, offset) {
    const peerMax = maxStreamPayload(this.#version, this.#sendFrameBudget(), id, offset);
    const compatibilityMax = BigInt(MAX_FRAME_PAYLOAD);
    const max = peerMax < compatibilityMax ? peerMax : compatibilityMax;
    if (max === 0n)
      throw new Error("peer frame limit leaves no room for stream data");
    return max;
  }
  /** Validate an encoded record against the peer's max_record_size (QMux01+). */
  #validateRecordSize(bytes) {
    if (usesRecords(this.#version)) {
      const limit = this.#paramsReceived ? this.#peerParams.maxRecordSize : DEFAULT_MAX_RECORD_SIZE;
      if (BigInt(bytes.byteLength) > limit) {
        throw new Error(`record exceeds peer max_record_size (${bytes.byteLength} > ${limit})`);
      }
    }
  }
  /** Encode and enqueue a stream-data/fin frame, resolving once it hits the wire. */
  async #enqueueStreamFrame(streamId, frame) {
    const scheduler = this.#scheduler;
    if (!scheduler)
      throw this.#closed ?? new Error("session not open");
    const bytes = encode5(frame, this.#version);
    this.#validateRecordSize(bytes);
    await scheduler.enqueueStream(streamId, bytes);
  }
  async #sendStreamDataWithFlowControl(id, streamId, data) {
    const flow = this.#streamFlow.get(streamId);
    if (!flow)
      throw new Error(`missing flow state for stream ${streamId}`);
    for (let offset = 0; offset < data.byteLength; ) {
      const remaining = BigInt(data.byteLength - offset);
      const payloadMax = this.#maxStreamPayload(id, flow.sendOffset);
      const chunkMax = Number(payloadMax < remaining ? payloadMax : remaining);
      const allowed = await this.#claimSendCredit(streamId, BigInt(chunkMax));
      const sendable = Number(allowed);
      const chunk = data.subarray(offset, offset + sendable);
      const sendOffset = flow.sendOffset;
      try {
        await this.#enqueueStreamFrame(streamId, {
          type: "stream",
          id,
          offset: sendOffset,
          data: chunk,
          fin: false
        });
      } catch (e) {
        if (sendable > 0) {
          const flow2 = this.#streamFlow.get(streamId);
          if (flow2)
            flow2.sendCredit.release(BigInt(sendable));
          this.#connCredit.release(BigInt(sendable));
        }
        throw e;
      }
      flow.sendOffset += BigInt(sendable);
      offset += sendable;
    }
  }
  async #sendStreamData(id, data) {
    const streamId = id.value.value;
    if (isQmux(this.#version)) {
      await this.#sendStreamDataWithFlowControl(id, streamId, data);
    } else {
      const chunkMax = Number(this.#maxStreamPayload(id, 0n));
      for (let offset = 0; offset < data.byteLength; offset += chunkMax) {
        const end = Math.min(offset + chunkMax, data.byteLength);
        const chunk = data.subarray(offset, end);
        await this.#enqueueStreamFrame(streamId, { type: "stream", id, data: chunk, fin: false });
      }
    }
  }
  /** Send the FIN. Routed through the stream's own queue so it stays ordered
   *  after that stream's data (not via the control lane, which would jump ahead). */
  async #sendStreamFin(id) {
    const streamId = id.value.value;
    const offset = isQmux(this.#version) ? this.#streamFlow.get(streamId)?.sendOffset : void 0;
    if (isQmux(this.#version) && offset === void 0) {
      throw new Error(`missing flow state for stream ${streamId}`);
    }
    await this.#enqueueStreamFrame(streamId, { type: "stream", id, offset, data: new Uint8Array(), fin: true });
  }
  /** Enqueue a DATAGRAM frame on the scheduler's bounded, lossy datagram lane —
   *  dropped under transport backpressure or once closed, rather than piling up
   *  on the (lossless, unbounded) control lane. Size/support checks happen in
   *  {@link Datagrams} before we get here. */
  #sendDatagram(data) {
    if (this.#closed)
      return;
    const bytes = encode5({ type: "datagram", data }, this.#version);
    this.#validateRecordSize(bytes);
    this.#scheduler?.enqueueDatagram(bytes);
  }
  #sendPriorityFrame(frame) {
    if (this.#closed)
      return;
    const bytes = encode5(frame, this.#version);
    this.#validateRecordSize(bytes);
    this.#scheduler?.enqueueControl(bytes);
  }
  /** Register a stream's initial send priority and expose a mutable `sendOrder`
   *  accessor on its writable (matching the W3C `WebTransportSendStream` API).
   *  Updating it re-prioritizes the stream's queued data immediately. */
  #attachSendOrder(writable, streamId, initial) {
    this.#scheduler?.setSendOrder(streamId, initial);
    let order = initial;
    Object.defineProperty(writable, "sendOrder", {
      configurable: true,
      enumerable: true,
      get: () => order,
      set: (value) => {
        order = value;
        this.#scheduler?.setSendOrder(streamId, value);
      }
    });
  }
  async createBidirectionalStream(options) {
    await this.ready;
    if (this.#closed) {
      throw this.#closed;
    }
    const sendOrder2 = options?.sendOrder ?? DEFAULT_SEND_ORDER;
    await this.#bidiStreamCredit.claim(1n);
    const streamId = Id.create(this.#nextBiStreamId++, Dir.Bi, this.#isServer);
    const streamIdVal = streamId.value.value;
    if (isQmux(this.#version)) {
      this.#streamFlow.set(streamIdVal, {
        sendCredit: new Credit(this.#peerParams.initialMaxStreamDataBidiRemote),
        sendOffset: 0n,
        recvMax: this.#ourParams.initialMaxStreamDataBidiLocal,
        recvOffset: 0n,
        recvConsumed: 0n
      });
    }
    const writer = new WritableStream({
      start: (controller) => {
        this.#sendStreams.set(streamIdVal, controller);
      },
      write: async (chunk) => {
        await this.#sendStreamData(streamId, chunk);
      },
      abort: (e) => {
        this.#scheduler?.dropStream(streamIdVal, e instanceof Error ? e : new Error("stream aborted"));
        this.#sendPriorityFrame({
          type: "reset_stream",
          id: streamId,
          code: VarInt.from(resetCode(e)),
          finalSize: this.#sendFinalSize(streamIdVal)
        });
        this.#sendStreams.delete(streamIdVal);
        this.#maybeDeleteStreamFlow(streamIdVal);
      },
      close: async () => {
        await this.#sendStreamFin(streamId);
        this.#sendStreams.delete(streamIdVal);
        this.#scheduler?.forget(streamIdVal);
        this.#maybeDeleteStreamFlow(streamIdVal);
      }
    });
    this.#attachSendOrder(writer, streamIdVal, sendOrder2);
    const recvStream = new RecvStream((bytes) => {
      this.#accountStreamConsumed(streamIdVal, bytes);
      this.#accountConnConsumed(bytes);
    }, (discarded, reason2) => {
      this.#accountConnConsumed(discarded);
      this.#sendPriorityFrame({
        type: "stop_sending",
        id: streamId,
        code: VarInt.from(resetCode(reason2))
      });
      this.#recvStreams.delete(streamIdVal);
      this.#maybeDeleteStreamFlow(streamIdVal);
    });
    this.#recvStreams.set(streamIdVal, recvStream);
    return { readable: recvStream.readable, writable: writer };
  }
  async createUnidirectionalStream(options) {
    await this.ready;
    if (this.#closed) {
      throw this.#closed;
    }
    const sendOrder2 = options?.sendOrder ?? DEFAULT_SEND_ORDER;
    await this.#uniStreamCredit.claim(1n);
    const streamId = Id.create(this.#nextUniStreamId++, Dir.Uni, this.#isServer);
    const streamIdVal = streamId.value.value;
    if (isQmux(this.#version)) {
      this.#streamFlow.set(streamIdVal, {
        sendCredit: new Credit(this.#peerParams.initialMaxStreamDataUni),
        sendOffset: 0n,
        recvMax: 0n,
        recvOffset: 0n,
        recvConsumed: 0n
      });
    }
    const session = this;
    const writer = new WritableStream({
      start: (controller) => {
        session.#sendStreams.set(streamIdVal, controller);
      },
      async write(chunk) {
        await session.#sendStreamData(streamId, chunk);
      },
      abort(e) {
        session.#scheduler?.dropStream(streamIdVal, e instanceof Error ? e : new Error("stream aborted"));
        session.#sendPriorityFrame({
          type: "reset_stream",
          id: streamId,
          code: VarInt.from(resetCode(e)),
          finalSize: session.#sendFinalSize(streamIdVal)
        });
        session.#sendStreams.delete(streamIdVal);
        session.#maybeDeleteStreamFlow(streamIdVal);
      },
      async close() {
        await session.#sendStreamFin(streamId);
        session.#sendStreams.delete(streamIdVal);
        session.#scheduler?.forget(streamIdVal);
        session.#maybeDeleteStreamFlow(streamIdVal);
      }
    });
    this.#attachSendOrder(writer, streamIdVal, sendOrder2);
    return writer;
  }
  /** Shared teardown for both terminal transitions: tears down streams,
   *  credits, and the scheduler. The caller has already marked the session
   *  closed and settled `ready`/`closed` — the only thing that differs between
   *  a clean shutdown and a dropped session is which way `closed` settled, and
   *  {@link #close} vs {@link #abort} own that choice. */
  #teardown() {
    if (this.#idleTimer) {
      clearInterval(this.#idleTimer);
      this.#idleTimer = void 0;
    }
    this.#incomingBidirectionalQueue.close(this.#closeReason);
    this.#incomingUnidirectionalQueue.close(this.#closeReason);
    this.datagrams.close(this.#closeReason);
    for (const c of this.#sendStreams.values()) {
      try {
        c.error(this.#closed);
      } catch {
      }
    }
    const closeErr = this.#closed ?? this.#closeReason ?? new Error("Connection closed");
    for (const recv of this.#recvStreams.values()) {
      try {
        recv.error(closeErr);
      } catch {
      }
    }
    this.#sendStreams.clear();
    this.#recvStreams.clear();
    for (const flow of this.#streamFlow.values()) {
      flow.sendCredit.close(closeErr);
    }
    this.#streamFlow.clear();
    this.#connCredit.close(closeErr);
    this.#bidiStreamCredit.close(closeErr);
    this.#uniStreamCredit.close(closeErr);
    this.#recvBiCredit.close(closeErr);
    this.#recvUniCredit.close(closeErr);
    this.#scheduler?.close(closeErr);
  }
  /** Graceful terminal transition: `closed` **fulfills** with the close code
   *  and reason. Per the WebTransport contract this is the only clean outcome —
   *  reached by a local {@link close} (which first puts an APPLICATION_CLOSE on
   *  the wire) or by receiving a peer's APPLICATION_CLOSE (0x1d). A peer's
   *  CONNECTION_CLOSE (0x1c) routes to {@link #abort} instead. Idempotent. */
  #close(code, reason2) {
    if (this.#closed)
      return;
    this.#closed = this.#closeReason ?? new Error(`Connection closed: ${code} ${reason2}`);
    this.#readyReject(this.#closed);
    this.#closedResolve({ closeCode: code, reason: reason2 });
    this.#teardown();
  }
  /** Abnormal terminal transition: `closed` **rejects** with a
   *  `WebTransportError`. Reached on everything that is not a clean shutdown —
   *  socket failure, read error, idle timeout, a protocol violation we detected
   *  locally, or a peer's CONNECTION_CLOSE (0x1c). Close codes cannot carry this
   *  distinction, since they are
   *  application-defined (an app closing with 1006 must not look like a dropped
   *  socket). `cause`, when given, is preserved as the close reason so the
   *  original failure survives on `closed`. Idempotent. */
  #abort(code, reason2, cause) {
    if (this.#closed)
      return;
    if (cause !== void 0) {
      this.#closeReason ??= cause instanceof Error ? cause : new Error(String(cause));
    }
    this.#closed = this.#closeReason ?? new Error(`Connection closed: ${code} ${reason2}`);
    this.#readyReject(this.#closed);
    this.#closedReject(new SessionError(this.#closed.message, { cause: this.#closed }));
    this.#teardown();
  }
  /** Tear down the underlying transport. The meaningful close code/reason
   *  already travels in the CONNECTION_CLOSE frame, so the WebSocket-level
   *  close is bare (avoids the WebSocket close-code validity constraints). */
  #transportClose() {
    try {
      this.#wss?.close();
    } catch {
    }
  }
  /** APPLICATION_CLOSE (0x1d): a graceful, app-initiated close the peer surfaces
   *  by *fulfilling* its `closed`. Pairs with the {@link #close} transition. */
  #sendApplicationClose(code, reason2) {
    this.#sendPriorityFrame({ type: "application_close", code: VarInt.from(code), reason: reason2 });
    setTimeout(() => {
      this.#transportClose();
    }, 100);
  }
  /** CONNECTION_CLOSE (0x1c): a protocol violation or transport error we detected,
   *  which the peer surfaces by *rejecting* its `closed`. Pairs with the
   *  {@link #abort} transition. */
  #sendConnectionClose(code, reason2) {
    this.#sendPriorityFrame({ type: "connection_close", code: VarInt.from(code), reason: reason2 });
    setTimeout(() => {
      this.#transportClose();
    }, 100);
  }
  close(info) {
    if (this.#closed)
      return;
    const code = info?.closeCode ?? 0;
    const reason2 = info?.reason ?? "";
    this.#sendApplicationClose(code, reason2);
    this.#close(code, reason2);
  }
  /** Resize the send-buffer high-water mark (bytes) used for write
   *  backpressure. A QMux extension beyond the standard `WebTransport` API:
   *  set this to roughly the bandwidth-delay product (RTT × estimated
   *  throughput) to keep the pipe full while leaving as much queued data as
   *  possible reprioritizable by the send scheduler.
   *
   *  No-op when a native `WebSocketStream` is in use (it sizes its own send
   *  buffer); effective with the `@moq/web-socket-stream` ponyfill fallback. */
  setSendBufferSize(bytes) {
    this.#sendBufferSize = Math.max(1, Math.floor(bytes));
    this.#wss?.setHighWaterMark?.(this.#sendBufferSize);
  }
  get congestionControl() {
    return "default";
  }
};

// scripts/moq-browser/node_modules/@moq/qmux/index.js
var qmux_default = Session;

// vendor/moq/js/net/src/connection/transport.ts
function transportOf(quic) {
  return quic instanceof qmux_default ? "websocket" : "webtransport";
}

// vendor/moq/js/net/src/lite/datagram_stream.ts
function datagrams(quic) {
  return quic.datagrams;
}
function maxDatagramSize(quic) {
  const size2 = datagrams(quic)?.maxDatagramSize;
  return typeof size2 === "number" && size2 > 0 ? size2 : 0;
}
function datagramReader(quic) {
  const readable = datagrams(quic)?.readable;
  if (!readable) {
    console.warn("datagram receive disabled: WebTransport datagrams.readable is unavailable");
    return void 0;
  }
  try {
    return readable.getReader();
  } catch (err) {
    console.warn("datagram receive disabled: failed to open WebTransport datagram reader", err);
    return void 0;
  }
}
function datagramWriter(quic) {
  const stream = datagrams(quic);
  if (!stream || maxDatagramSize(quic) === 0) return void 0;
  try {
    const writable = typeof stream.createWritable === "function" ? stream.createWritable() : stream.writable;
    if (!writable) {
      console.warn("datagram send disabled: WebTransport datagram writable stream is unavailable");
      return void 0;
    }
    return writable.getWriter();
  } catch (err) {
    console.warn("datagram send disabled: failed to open WebTransport datagram writer", err);
    return void 0;
  }
}

// vendor/moq/js/net/src/ietf/message.ts
async function encode6(writer, f) {
  let scratch = new Uint8Array();
  const temp = new Writer(
    new WritableStream({
      write(chunk) {
        const needed = scratch.byteLength + chunk.byteLength;
        if (needed > scratch.buffer.byteLength) {
          const capacity = Math.max(needed, scratch.buffer.byteLength * 2);
          const newBuffer = new ArrayBuffer(capacity);
          const newScratch = new Uint8Array(newBuffer, 0, needed);
          newScratch.set(scratch);
          newScratch.set(chunk, scratch.byteLength);
          scratch = newScratch;
        } else {
          scratch = new Uint8Array(scratch.buffer, 0, needed);
          scratch.set(chunk, needed - chunk.byteLength);
        }
      }
    }),
    writer.version
  );
  try {
    await f(temp);
  } finally {
    temp.close();
  }
  await temp.closed;
  if (scratch.byteLength > 65535) {
    throw new Error(`Message too large: ${scratch.byteLength} bytes (max 65535)`);
  }
  await writer.u16(scratch.byteLength);
  await writer.write(scratch);
}
async function decode6(reader, f) {
  const size2 = await reader.u16();
  const data = await reader.read(size2);
  const limit = new Reader(void 0, data, reader.version);
  const msg = await f(limit);
  if (!await limit.done()) {
    throw new Error("Message decoding consumed too few bytes");
  }
  return msg;
}

// vendor/moq/js/net/src/ietf/goaway.ts
var GoAway = class _GoAway {
  static id = 16;
  newSessionUri;
  timeout;
  constructor({ newSessionUri, timeout = 0n }) {
    this.newSessionUri = newSessionUri;
    this.timeout = timeout;
  }
  async #encode(w, version2) {
    await w.string(this.newSessionUri);
    if (version2 !== Version.DRAFT_14 && version2 !== Version.DRAFT_15 && version2 !== Version.DRAFT_16) {
      await w.u62(this.timeout);
    }
  }
  async encode(w, version2) {
    return encode6(w, (mw) => this.#encode(mw, version2));
  }
  static async decode(r, version2) {
    return decode6(r, (mr) => _GoAway.#decode(mr, version2));
  }
  static async #decode(r, version2) {
    const newSessionUri = await r.string();
    const timeout = version2 === Version.DRAFT_14 || version2 === Version.DRAFT_15 || version2 === Version.DRAFT_16 ? 0n : await r.u62();
    return new _GoAway({ newSessionUri, timeout });
  }
};

// vendor/moq/js/net/src/ietf/object.ts
var GROUP_END = 3;
var PROP_TIMESCALE = 0x08n;
var PROP_TIMESTAMP = 0x10n;
var PROP_TIMESTAMP_DRAFT03 = 0x06n;
var FIRST_OBJECT_BIT = 64;
function hasFirstObjectBit(version2) {
  switch (version2) {
    case Version.DRAFT_14:
    case Version.DRAFT_15:
    case Version.DRAFT_16:
    case Version.DRAFT_17:
      return false;
    default:
      return true;
  }
}
function hasDeltaObjectPropertyTypes(version2) {
  switch (version2) {
    case Version.DRAFT_14:
    case Version.DRAFT_15:
      return false;
    default:
      return true;
  }
}
async function encodeObjectPropertyType(w, id, prev, version2) {
  const encoded = hasDeltaObjectPropertyTypes(version2) ? id - prev : id;
  await w.u62(encoded);
}
async function encodeObjectTime(w, timestamp, timescale, version2) {
  const value = Math.round(timestamp.value * timescale / timestamp.scale);
  await encodeObjectPropertyType(w, PROP_TIMESTAMP, 0n, version2);
  await w.u62(BigInt(value));
}
async function encodeObjectExtensions(timestamp, timescale, version2) {
  if (timestamp === void 0) {
    return new Uint8Array();
  }
  const chunks = [];
  const writer = new Writer(
    new WritableStream({
      write(chunk) {
        chunks.push(new Uint8Array(chunk));
      }
    }),
    version2
  );
  await encodeObjectTime(writer, timestamp, timescale, version2);
  writer.close();
  await writer.closed;
  const size2 = chunks.reduce((total, chunk) => total + chunk.byteLength, 0);
  const result = new Uint8Array(size2);
  let offset = 0;
  for (const chunk of chunks) {
    result.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return result;
}
async function decodeObjectTime(r, timescale, version2) {
  let timestamp;
  let overrideScale;
  let prevType = 0n;
  let first = true;
  while (!await r.done()) {
    const step = await r.u62();
    const id = !hasDeltaObjectPropertyTypes(version2) || first ? step : prevType + step;
    first = false;
    prevType = id;
    if (id % 2n === 0n) {
      const value = await r.u62();
      if (id === PROP_TIMESTAMP || id === PROP_TIMESTAMP_DRAFT03) {
        timestamp = value;
      } else if (id === PROP_TIMESCALE) {
        overrideScale = value;
      }
    } else {
      const size2 = await r.u53();
      await r.read(size2);
    }
  }
  if (timestamp === void 0) {
    return void 0;
  }
  return new Timestamp(Number(timestamp), overrideScale !== void 0 ? Timescale(Number(overrideScale)) : timescale);
}
var Group = class _Group {
  flags;
  trackAlias;
  groupId;
  subGroupId;
  publisherPriority;
  constructor({
    trackAlias,
    groupId,
    subGroupId,
    publisherPriority,
    flags
  }) {
    this.flags = flags;
    this.trackAlias = trackAlias;
    this.groupId = groupId;
    this.subGroupId = subGroupId;
    this.publisherPriority = publisherPriority;
  }
  async encode(w, version2) {
    if (!this.flags.hasSubgroup && this.subGroupId !== 0) {
      throw new Error(`Subgroup ID must be 0 if hasSubgroup is false: ${this.subGroupId}`);
    }
    const base = this.flags.hasPriority ? 16 : 48;
    let id = base;
    if (this.flags.hasExtensions) {
      id |= 1;
    }
    if (this.flags.hasSubgroupObject) {
      id |= 2;
    }
    if (this.flags.hasSubgroup) {
      id |= 4;
    }
    if (this.flags.hasEnd) {
      id |= 8;
    }
    if (this.flags.firstObject && hasFirstObjectBit(version2)) {
      id |= FIRST_OBJECT_BIT;
    }
    await w.u53(id);
    await w.u62(this.trackAlias);
    await w.u53(this.groupId);
    if (this.flags.hasSubgroup) {
      await w.u53(this.subGroupId);
    }
    if (this.flags.hasPriority) {
      await w.u8(this.publisherPriority);
    }
  }
  static async decode(r, version2) {
    const raw = await r.u53();
    const legacy = !hasFirstObjectBit(version2);
    const firstObject = legacy || (raw & FIRST_OBJECT_BIT) !== 0;
    const id = legacy ? raw : raw & ~FIRST_OBJECT_BIT;
    let hasPriority;
    let baseId;
    if (id >= 16 && id <= 31) {
      hasPriority = true;
      baseId = id;
    } else if (id >= 48 && id <= 63) {
      hasPriority = false;
      baseId = id - (48 - 16);
    } else {
      throw new Error(`Unsupported group type: ${id}`);
    }
    const flags = {
      hasExtensions: (baseId & 1) !== 0,
      hasSubgroupObject: (baseId & 2) !== 0,
      hasSubgroup: (baseId & 4) !== 0,
      hasEnd: (baseId & 8) !== 0,
      hasPriority,
      firstObject
    };
    const trackAlias = await r.u62();
    const groupId = await r.u53();
    const subGroupId = flags.hasSubgroup ? await r.u53() : 0;
    const publisherPriority = hasPriority ? await r.u8() : 128;
    return new _Group({ trackAlias, groupId, subGroupId, publisherPriority, flags });
  }
};
var Frame = class _Frame {
  /** The object payload, or `undefined` for the end of group marker. */
  payload;
  /** The presentation timestamp carried in object properties, when present. */
  timestamp;
  constructor({ payload, timestamp } = {}) {
    this.payload = payload;
    this.timestamp = timestamp;
  }
  /**
   * Encode this frame using the group flags and negotiated IETF version.
   *
   * `idDelta` is the first object's absolute Object ID and zero for every later one, so a
   * group whose head was trimmed by a filter still puts the true numbering on the wire.
   */
  async encode(w, flags, timescale, version2 = w.version, idDelta = 0) {
    await w.u53(idDelta);
    if (flags.hasExtensions) {
      const extensions = await encodeObjectExtensions(this.timestamp, timescale, version2);
      await w.u53(extensions.byteLength);
      await w.write(extensions);
    }
    if (this.payload !== void 0) {
      await w.u53(this.payload.byteLength);
      if (this.payload.byteLength === 0) {
        await w.u53(0);
      } else {
        await w.write(this.payload);
      }
    } else {
      await w.u53(0);
      await w.u53(GROUP_END);
    }
  }
  /** Decode a frame using the group flags and negotiated IETF version. */
  static async decode(r, flags, timescale, version2 = r.version) {
    const delta = await r.u53();
    if (delta !== 0) {
      throw new Error(`object IDs must start at 0 and increment by 1, got a delta of ${delta}`);
    }
    let timestamp;
    if (flags.hasExtensions) {
      const extensionsLength = await r.u53();
      const extensions = await r.read(extensionsLength);
      if (timescale !== void 0) {
        timestamp = await decodeObjectTime(new Reader(void 0, extensions, version2), timescale, version2);
      }
    }
    const payloadLength = await r.u53();
    if (payloadLength > 0) {
      const payload = await r.read(payloadLength);
      return new _Frame({ payload, timestamp });
    }
    const status = await r.u53();
    if (flags.hasEnd) {
      if (status === 0) return new _Frame({ payload: new Uint8Array(0), timestamp });
    } else if (status === 0 || status === GROUP_END) {
      return new _Frame();
    }
    throw new Error(`Unsupported object status: ${status}`);
  }
};
var FETCH_OBJECT_ID = 4;
var FETCH_GROUP_ID = 8;
var FETCH_PRIORITY = 16;
var FETCH_PROPERTIES = 32;
var FetchFrame = class {
  /** The object payload. */
  payload;
  /** The presentation timestamp carried in object properties, when present. */
  timestamp;
  constructor({ payload, timestamp }) {
    this.payload = payload;
    this.timestamp = timestamp;
  }
  /** Encode this object at `position`, stamping it in the track's timescale. */
  async encode(w, position, timescale, version2 = w.version) {
    if (position.first) {
      const properties = this.timestamp !== void 0 ? FETCH_PROPERTIES : 0;
      await w.u53(FETCH_GROUP_ID | FETCH_OBJECT_ID | FETCH_PRIORITY | properties);
      await w.u53(position.group);
      await w.u53(position.object);
      await w.u8(0);
    } else {
      await w.u53(this.timestamp !== void 0 ? FETCH_PROPERTIES : 0);
    }
    if (this.timestamp !== void 0) {
      const extensions = await encodeObjectExtensions(this.timestamp, timescale, version2);
      await w.u53(extensions.byteLength);
      await w.write(extensions);
    }
    await w.u53(this.payload.byteLength);
    if (this.payload.byteLength > 0) {
      await w.write(this.payload);
    }
  }
};

// vendor/moq/js/net/src/ietf/properties.ts
var TIMESCALE = 0x08n;
var DEFAULT_PUBLISHER_GROUP_ORDER = 0x22n;
var DESCENDING = 2;
async function encode7(w, properties, version2) {
  if (version2 === Version.DRAFT_14 || version2 === Version.DRAFT_15 || version2 === Version.DRAFT_16) {
    return;
  }
  let prevType = 0n;
  if (properties.timescale !== void 0) {
    await w.u62(TIMESCALE);
    await w.u62(BigInt(properties.timescale));
    prevType = TIMESCALE;
  }
  if (properties.groupOrder !== void 0) {
    await w.u62(DEFAULT_PUBLISHER_GROUP_ORDER - prevType);
    await w.u62(BigInt(properties.groupOrder));
  }
}
async function decode7(r, version2) {
  if (version2 === Version.DRAFT_14 || version2 === Version.DRAFT_15) {
    return {};
  }
  const properties = {};
  let prevType = 0n;
  let i = 0;
  while (!await r.done()) {
    const delta = await r.u62();
    const abs = i === 0 ? delta : prevType + delta;
    prevType = abs;
    i++;
    if (abs % 2n === 0n) {
      const value = await r.u62();
      if (abs === TIMESCALE && value > 0n) {
        properties.timescale = Timescale(Number(value));
      } else if (abs === DEFAULT_PUBLISHER_GROUP_ORDER) {
        if (value < 1n || value > 2n) {
          throw new Error(`unknown group order: ${value}`);
        }
        properties.groupOrder = Number(value);
      }
    } else {
      const len = await r.u53();
      await r.read(len);
    }
  }
  return properties;
}

// vendor/moq/js/net/src/ietf/publish.ts
var Publish = class _Publish {
  static id = 29;
  requestId;
  trackNamespace;
  trackName;
  trackAlias;
  groupOrder;
  contentExists;
  largest;
  forward;
  constructor({
    requestId,
    trackNamespace,
    trackName,
    trackAlias,
    groupOrder,
    contentExists,
    largest,
    forward
  }) {
    this.requestId = requestId;
    this.trackNamespace = trackNamespace;
    this.trackName = trackName;
    this.trackAlias = trackAlias;
    this.groupOrder = groupOrder;
    this.contentExists = contentExists;
    this.largest = largest;
    this.forward = forward;
  }
  async #encode(w, version2) {
    await w.u62(this.requestId);
    if (version2 === Version.DRAFT_17) {
      await w.u62(0n);
    }
    await encode3(w, this.trackNamespace);
    await w.string(this.trackName);
    await w.u62(this.trackAlias);
    if (version2 === Version.DRAFT_14) {
      await w.u8(this.groupOrder);
      await w.bool(this.contentExists);
      if (this.contentExists !== !!this.largest) {
        throw new Error("contentExists and largest must both be true or false");
      }
      if (this.largest) {
        await w.u62(this.largest.groupId);
        await w.u62(this.largest.objectId);
      }
      await w.bool(this.forward);
      await w.u53(0);
    } else {
      if (this.contentExists !== !!this.largest) {
        throw new Error("contentExists and largest must both be true or false");
      }
      const params = new Parameters();
      if (version2 === Version.DRAFT_15) {
        params.groupOrder = this.groupOrder;
      }
      params.forward = this.forward;
      if (this.largest) {
        params.largest = this.largest;
      }
      await params.encode(w, version2);
      await encode7(w, { groupOrder: this.groupOrder }, version2);
    }
  }
  async encode(w, version2) {
    return encode6(w, (mw) => this.#encode(mw, version2));
  }
  static async decode(r, version2) {
    return decode6(r, (mr) => _Publish.#decode(mr, version2));
  }
  static async #decode(r, version2) {
    const requestId = await r.u62();
    if (version2 === Version.DRAFT_17) {
      await r.u62();
    }
    const trackNamespace = await decode3(r);
    const trackName = await r.string();
    const trackAlias = await r.u62();
    if (version2 === Version.DRAFT_14) {
      const groupOrder2 = await r.u8();
      const contentExists = await r.bool();
      const largest2 = contentExists ? { groupId: await r.u62(), objectId: await r.u62() } : void 0;
      const forward2 = await r.bool();
      await Parameters.decode(r, version2);
      return new _Publish({
        requestId,
        trackNamespace,
        trackName,
        trackAlias,
        groupOrder: groupOrder2,
        contentExists,
        largest: largest2,
        forward: forward2
      });
    }
    const params = await Parameters.decode(r, version2);
    const properties = await decode7(r, version2);
    const groupOrder = properties.groupOrder ?? params.groupOrder ?? 2;
    const forward = params.forward ?? true;
    const largest = params.largest;
    return new _Publish({
      requestId,
      trackNamespace,
      trackName,
      trackAlias,
      groupOrder,
      contentExists: !!largest,
      largest,
      forward
    });
  }
};
var PublishError = class _PublishError {
  static id = 31;
  requestId;
  errorCode;
  reasonPhrase;
  constructor({
    requestId,
    errorCode,
    reasonPhrase
  }) {
    this.requestId = requestId;
    this.errorCode = errorCode;
    this.reasonPhrase = reasonPhrase;
  }
  async #encode(w) {
    await w.u62(this.requestId);
    await w.u62(BigInt(this.errorCode));
    await w.string(this.reasonPhrase);
  }
  async encode(w, _version) {
    return encode6(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode6(r, _PublishError.#decode);
  }
  static async #decode(r) {
    const requestId = await r.u62();
    const errorCode = Number(await r.u62());
    const reasonPhrase = await r.string();
    return new _PublishError({ requestId, errorCode, reasonPhrase });
  }
};
var PublishDone = class _PublishDone {
  static id = 11;
  requestId;
  statusCode;
  reasonPhrase;
  constructor({
    requestId,
    statusCode,
    reasonPhrase
  }) {
    this.requestId = requestId;
    this.statusCode = statusCode;
    this.reasonPhrase = reasonPhrase;
  }
  async #encode(w, version2) {
    if (version2 === Version.DRAFT_14 || version2 === Version.DRAFT_15 || version2 === Version.DRAFT_16) {
      if (this.requestId === void 0) throw new Error("requestId required for draft14-16");
      await w.u62(this.requestId);
    }
    await w.u62(BigInt(this.statusCode));
    await w.u62(BigInt(0));
    await w.string(this.reasonPhrase);
  }
  async encode(w, version2) {
    return encode6(w, (mw) => this.#encode(mw, version2));
  }
  static async decode(r, version2) {
    return decode6(r, (mr) => _PublishDone.#decode(mr, version2));
  }
  static async #decode(r, version2) {
    const requestId = version2 === Version.DRAFT_14 || version2 === Version.DRAFT_15 || version2 === Version.DRAFT_16 ? await r.u62() : void 0;
    const statusCode = Number(await r.u62());
    await r.u62();
    const reasonPhrase = await r.string();
    return new _PublishDone({ requestId, statusCode, reasonPhrase });
  }
};

// vendor/moq/js/net/src/ietf/publish_namespace.ts
var PublishNamespace = class _PublishNamespace {
  static id = 6;
  requestId;
  trackNamespace;
  /** The MoQ Cluster parameters (see {@link Cluster}). Set on a session that negotiated
   * the extension and `undefined` on one that did not, which is what decides whether they
   * appear on the wire at all. */
  cluster;
  constructor({
    requestId,
    trackNamespace,
    cluster
  }) {
    this.requestId = requestId;
    this.trackNamespace = trackNamespace;
    this.cluster = cluster;
  }
  async #encode(w, version2) {
    await w.u62(this.requestId);
    if (version2 === Version.DRAFT_17) {
      await w.u62(0n);
    }
    await encode3(w, this.trackNamespace);
    const params = this.cluster ? intoParams(this.cluster) : new Parameters();
    await params.encode(w, version2);
  }
  async encode(w, version2) {
    return encode6(w, (wr) => this.#encode(wr, version2));
  }
  /**
   * Decode the message, expecting the cluster parameters when the session negotiated the
   * extension.
   *
   * The negotiation is session state rather than anything in the message, so the caller
   * supplies it. A negotiated session that omits HOP_PATH is a protocol violation, which
   * surfaces here as a throw.
   */
  static async decode(r, version2, negotiated2 = false) {
    return decode6(r, (rd) => _PublishNamespace.#decode(rd, version2, negotiated2));
  }
  static async #decode(r, version2, negotiated2) {
    const requestId = await r.u62();
    if (version2 === Version.DRAFT_17) {
      await r.u62();
    }
    const trackNamespace = await decode3(r);
    if (negotiated2) {
      const cluster = await decodeParams(r, version2);
      return new _PublishNamespace({ requestId, trackNamespace, cluster });
    }
    await Parameters.decode(r, version2);
    return new _PublishNamespace({ requestId, trackNamespace });
  }
};
var PublishNamespaceOk = class _PublishNamespaceOk {
  static id = 7;
  requestId;
  constructor({ requestId }) {
    this.requestId = requestId;
  }
  async #encode(w) {
    await w.u62(this.requestId);
  }
  async encode(w, _version) {
    return encode6(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode6(r, _PublishNamespaceOk.#decode);
  }
  static async #decode(r) {
    const requestId = await r.u62();
    return new _PublishNamespaceOk({ requestId });
  }
};
var PublishNamespaceError = class _PublishNamespaceError {
  static id = 8;
  requestId;
  errorCode;
  reasonPhrase;
  constructor({
    requestId,
    errorCode,
    reasonPhrase
  }) {
    this.requestId = requestId;
    this.errorCode = errorCode;
    this.reasonPhrase = reasonPhrase;
  }
  async #encode(w) {
    await w.u62(this.requestId);
    await w.u62(BigInt(this.errorCode));
    await w.string(this.reasonPhrase);
  }
  async encode(w, _version) {
    return encode6(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode6(r, _PublishNamespaceError.#decode);
  }
  static async #decode(r) {
    const requestId = await r.u62();
    const errorCode = Number(await r.u62());
    const reasonPhrase = await r.string();
    return new _PublishNamespaceError({ requestId, errorCode, reasonPhrase });
  }
};
var PublishNamespaceDone = class _PublishNamespaceDone {
  static id = 9;
  trackNamespace;
  requestId;
  // v16: uses request_id instead of track_namespace
  constructor({
    trackNamespace = "",
    requestId = 0n
  } = {}) {
    this.trackNamespace = trackNamespace;
    this.requestId = requestId;
  }
  async #encode(w, version2) {
    if (version2 !== Version.DRAFT_14 && version2 !== Version.DRAFT_15 && version2 !== Version.DRAFT_16) {
      throw new Error("PublishNamespaceDone removed in draft-17+");
    }
    if (version2 === Version.DRAFT_16) {
      await w.u62(this.requestId);
    } else {
      await encode3(w, this.trackNamespace);
    }
  }
  async encode(w, version2) {
    return encode6(w, (wr) => this.#encode(wr, version2));
  }
  static async decode(r, version2) {
    return decode6(r, (rd) => _PublishNamespaceDone.#decode(rd, version2));
  }
  static async #decode(r, version2) {
    if (version2 !== Version.DRAFT_14 && version2 !== Version.DRAFT_15 && version2 !== Version.DRAFT_16) {
      throw new Error("PublishNamespaceDone removed in draft-17+");
    }
    if (version2 === Version.DRAFT_16) {
      const requestId = await r.u62();
      return new _PublishNamespaceDone({ requestId });
    }
    const trackNamespace = await decode3(r);
    return new _PublishNamespaceDone({ trackNamespace });
  }
};

// vendor/moq/js/net/src/ietf/datagram.ts
var PROPERTIES = 1;
var END_OF_GROUP = 2;
var ZERO_OBJECT_ID = 4;
var DEFAULT_PRIORITY = 8;
var RESERVED = 16;
var STATUS = 32;
var KNOWN = PROPERTIES | END_OF_GROUP | ZERO_OBJECT_ID | DEFAULT_PRIORITY | STATUS;
var SINGLE_OBJECT_TYPE = END_OF_GROUP | ZERO_OBJECT_ID | DEFAULT_PRIORITY;
function leadingOnes(version2) {
  return version2 !== Version.DRAFT_14 && version2 !== Version.DRAFT_15 && version2 !== Version.DRAFT_16;
}
function encodeInt(v, version2) {
  return leadingOnes(version2) ? encodeLeadingOnes(v) : encode2(v);
}
function decodeInt(buf, version2) {
  return leadingOnes(version2) ? decodeLeadingOnes(buf) : decodeBigInt(buf);
}
function encodeObjectDatagram(trackAlias, groupId, payload, version2) {
  const type = encodeInt(SINGLE_OBJECT_TYPE, version2);
  const alias = encodeInt(trackAlias, version2);
  const group = encodeInt(groupId, version2);
  const out = new Uint8Array(type.byteLength + alias.byteLength + group.byteLength + payload.byteLength);
  let offset = 0;
  for (const part of [type, alias, group, payload]) {
    out.set(part, offset);
    offset += part.byteLength;
  }
  return out;
}
function decodeObjectDatagram(data, version2) {
  let rest = data;
  let value;
  [value, rest] = decodeInt(rest, version2);
  const type = Number(value);
  if (value > 0x7fn || type & RESERVED || type & ~KNOWN) throw new Error("invalid object datagram type");
  if (type & STATUS && type & END_OF_GROUP) throw new Error("invalid object datagram type");
  const [trackAlias, afterAlias] = decodeInt(rest, version2);
  const [groupId, afterGroup] = decodeInt(afterAlias, version2);
  rest = afterGroup;
  let objectId = 0n;
  if (!(type & ZERO_OBJECT_ID)) [objectId, rest] = decodeInt(rest, version2);
  if (!(type & DEFAULT_PRIORITY)) {
    if (rest.byteLength < 1) throw new Error("short object datagram");
    rest = rest.subarray(1);
  }
  if (type & PROPERTIES) {
    const [length, afterLength] = decodeInt(rest, version2);
    if (length === 0n || length > BigInt(afterLength.byteLength)) throw new Error("invalid object datagram properties");
    rest = afterLength.subarray(Number(length));
  }
  if (type & STATUS) {
    return { trackAlias, groupId, objectId, endOfGroup: false };
  }
  return { trackAlias, groupId, objectId, endOfGroup: (type & END_OF_GROUP) !== 0, payload: rest };
}

// vendor/moq/js/net/src/ietf/fetch.ts
var FetchHeader = class _FetchHeader {
  /** The uni stream type, not a control message id. */
  static type = 5;
  /** The SUBSCRIBE (or FETCH) request this stream carries objects for. */
  requestId;
  constructor({ requestId }) {
    this.requestId = requestId;
  }
  /** Write the header, which the stream type must already precede. */
  async encode(w, _version) {
    await w.u62(this.requestId);
  }
  /** Read the header, with the stream type already consumed. */
  static async decode(r, _version) {
    return new _FetchHeader({ requestId: await r.u62() });
  }
};

// vendor/moq/js/net/src/ietf/filter.ts
var TAG_NEXT_GROUP = 0x1n;
var TAG_LARGEST_OBJECT = 0x2n;
var TAG_ABSOLUTE_START = 0x3n;
var TAG_ABSOLUTE_RANGE = 0x4n;
function isDraft20(version2) {
  return version2 !== Version.DRAFT_14 && version2 !== Version.DRAFT_15 && version2 !== Version.DRAFT_16 && version2 !== Version.DRAFT_17 && version2 !== Version.DRAFT_18 && version2 !== Version.DRAFT_19;
}
function usesLeadingOnes(version2) {
  return version2 !== Version.DRAFT_14 && version2 !== Version.DRAFT_15 && version2 !== Version.DRAFT_16;
}
function varint(v, version2) {
  return usesLeadingOnes(version2) ? encodeLeadingOnes(v) : encode2(v);
}
function unvarint(buf, version2) {
  return usesLeadingOnes(version2) ? decodeLeadingOnes(buf) : decodeBigInt(buf);
}
function concat(parts2) {
  const total = parts2.reduce((n, p) => n + p.length, 0);
  const out = new Uint8Array(total);
  let offset = 0;
  for (const part of parts2) {
    out.set(part, offset);
    offset += part.length;
  }
  return out;
}
function endDelta(startGroup, endGroup) {
  if (endGroup < startGroup) {
    throw new Error(`filter range runs backwards: ${startGroup} > ${endGroup}`);
  }
  return endGroup - startGroup;
}
function encode8(filter, version2) {
  return isDraft20(version2) ? encodeFields(filter, version2) : encodeTag(filter, version2);
}
function encodeFields(filter, version2) {
  switch (filter.kind) {
    case "unfiltered":
      return new Uint8Array();
    case "nextObject":
      return concat([varint(0n, version2), varint(0n, version2)]);
    case "relative":
      return varint(filter.groups, version2);
    case "absolute": {
      if (filter.startGroup === 0n && filter.startObject === 0n && filter.endGroup === void 0) {
        return new Uint8Array();
      }
      const parts2 = [varint(filter.startGroup, version2), varint(filter.startObject, version2)];
      if (filter.endGroup !== void 0) {
        parts2.push(varint(endDelta(filter.startGroup, filter.endGroup), version2));
        if (filter.endObject !== void 0) {
          parts2.push(varint(filter.endObject, version2));
        }
      }
      return concat(parts2);
    }
  }
}
function encodeTag(filter, version2) {
  switch (filter.kind) {
    case "unfiltered":
      return concat([varint(TAG_ABSOLUTE_START, version2), varint(0n, version2), varint(0n, version2)]);
    case "nextObject":
      return varint(TAG_LARGEST_OBJECT, version2);
    case "relative":
      if (filter.groups !== 0n) {
        throw new Error(`relative filter ${filter.groups} needs draft-20`);
      }
      return varint(TAG_NEXT_GROUP, version2);
    case "absolute": {
      if (filter.endObject !== void 0) {
        throw new Error("an object-bounded range needs draft-20");
      }
      const parts2 = [
        varint(filter.endGroup === void 0 ? TAG_ABSOLUTE_START : TAG_ABSOLUTE_RANGE, version2),
        varint(filter.startGroup, version2),
        varint(filter.startObject, version2)
      ];
      if (filter.endGroup !== void 0) {
        parts2.push(varint(endDelta(filter.startGroup, filter.endGroup), version2));
      }
      return concat(parts2);
    }
  }
}
function decode8(data, version2) {
  return isDraft20(version2) ? decodeFields(data, version2) : decodeTag(data, version2);
}
function decodeFields(data, version2) {
  const fields = [];
  let rest = data;
  while (rest.length > 0) {
    if (fields.length === 4) {
      throw new Error("too many fields in LOCATION_FILTER");
    }
    const [value, next] = unvarint(rest, version2);
    fields.push(value);
    rest = next;
  }
  switch (fields.length) {
    case 0:
      return { kind: "unfiltered" };
    case 1:
      return { kind: "relative", groups: fields[0] };
    default: {
      const [startGroup, startObject] = fields;
      if (fields.length === 2 && startGroup === 0n && startObject === 0n) {
        return { kind: "nextObject" };
      }
      const endGroup = fields.length >= 3 ? startGroup + fields[2] : void 0;
      const endObject = fields.length === 4 ? fields[3] : void 0;
      return { kind: "absolute", startGroup, startObject, endGroup, endObject };
    }
  }
}
function decodeTag(data, version2) {
  const [tag, afterTag] = unvarint(data, version2);
  const readLocation = (buf) => {
    const [group, afterGroup] = unvarint(buf, version2);
    const [object, rest] = unvarint(afterGroup, version2);
    return [group, object, rest];
  };
  switch (tag) {
    case TAG_NEXT_GROUP:
      expectEmpty(afterTag);
      return { kind: "relative", groups: 0n };
    case TAG_LARGEST_OBJECT:
      expectEmpty(afterTag);
      return { kind: "nextObject" };
    case TAG_ABSOLUTE_START: {
      const [startGroup, startObject, rest] = readLocation(afterTag);
      expectEmpty(rest);
      return { kind: "absolute", startGroup, startObject };
    }
    case TAG_ABSOLUTE_RANGE: {
      const [startGroup, startObject, afterLocation] = readLocation(afterTag);
      const [delta, rest] = unvarint(afterLocation, version2);
      expectEmpty(rest);
      return { kind: "absolute", startGroup, startObject, endGroup: startGroup + delta };
    }
    default:
      throw new Error(`unsupported filter type: ${tag}`);
  }
}
async function decodeInline(r) {
  const tag = await r.u62();
  switch (tag) {
    case TAG_NEXT_GROUP:
      return { kind: "relative", groups: 0n };
    case TAG_LARGEST_OBJECT:
      return { kind: "nextObject" };
    case TAG_ABSOLUTE_START:
      return { kind: "absolute", startGroup: await r.u62(), startObject: await r.u62() };
    case TAG_ABSOLUTE_RANGE: {
      const startGroup = await r.u62();
      const startObject = await r.u62();
      const delta = await r.u62();
      return { kind: "absolute", startGroup, startObject, endGroup: startGroup + delta };
    }
    default:
      throw new Error(`unsupported filter type: ${tag}`);
  }
}
function expectEmpty(rest) {
  if (rest.length !== 0) {
    throw new Error("trailing bytes in LOCATION_FILTER");
  }
}
var FILL_LOCATION_FILTER = 0x21n;
var FILL_ALLOWED = /* @__PURE__ */ new Map([
  [0x0an, "varint"],
  // FILL_TIMEOUT
  [0x20n, "byte"],
  // SUBSCRIBER_PRIORITY, a uint8
  [FILL_LOCATION_FILTER, "bytes"],
  [0x22n, "byte"],
  // GROUP_ORDER, a uint8
  [0x25n, "bytes"],
  // SUBGROUP_FILTER
  [0x26n, "bytes"],
  // OBJECTID_FILTER, length prefixed despite an even id
  [0x27n, "bytes"],
  // PRIORITY_FILTER
  [0x28n, "bytes"]
  // OBJECT_PROPERTY_FILTER, likewise
]);
function encodeFill(fill, version2) {
  if (fill.filter === void 0) {
    return varint(0n, version2);
  }
  return concat([
    varint(1n, version2),
    // The first type in a scope is not delta encoded, so this is the raw id.
    varint(FILL_LOCATION_FILTER, version2),
    encodeLengthPrefixed(encode8(fill.filter, version2), version2)
  ]);
}
function decodeFill(data, version2) {
  const [count, afterCount] = unvarint(data, version2);
  if (count > 64n) {
    throw new Error("too many parameters in FILL_PARAMETERS");
  }
  let rest = afterCount;
  let filter;
  let rangeFilters = false;
  let prev = 0n;
  for (let i = 0n; i < count; i++) {
    const [delta, afterType] = unvarint(rest, version2);
    const key = i === 0n ? delta : prev + delta;
    prev = key;
    rest = afterType;
    const framing = FILL_ALLOWED.get(key);
    if (framing === void 0) {
      throw new Error(`parameter ${key} is not allowed inside FILL_PARAMETERS`);
    }
    rangeFilters ||= key >= 0x25n && key <= 0x28n;
    if (framing === "varint") {
      [, rest] = unvarint(rest, version2);
      continue;
    }
    if (framing === "byte") {
      if (rest.length < 1) throw new Error("truncated value inside FILL_PARAMETERS");
      rest = rest.slice(1);
      continue;
    }
    const [length, afterLength] = unvarint(rest, version2);
    if (BigInt(afterLength.length) < length) {
      throw new Error("truncated value inside FILL_PARAMETERS");
    }
    const value = afterLength.slice(0, Number(length));
    rest = afterLength.slice(Number(length));
    if (key === FILL_LOCATION_FILTER) {
      if (filter !== void 0) {
        throw new Error("duplicate LOCATION_FILTER inside FILL_PARAMETERS");
      }
      filter = decode8(value, version2);
    }
  }
  if (rest.length !== 0) {
    throw new Error("trailing bytes in FILL_PARAMETERS");
  }
  return { filter, rangeFilters };
}
function encodeLengthPrefixed(value, version2) {
  return concat([varint(BigInt(value.length), version2), value]);
}

// vendor/moq/js/net/src/ietf/priority.ts
function fromWire(priority) {
  return 255 - priority;
}
function toWire(priority) {
  return 255 - priority;
}

// vendor/moq/js/net/src/ietf/request.ts
var RequestOk = class _RequestOk {
  static id = 7;
  requestId;
  parameters;
  constructor({ requestId, parameters = new Parameters() }) {
    this.requestId = requestId;
    this.parameters = parameters;
  }
  async #encode(w, version2) {
    if (version2 === Version.DRAFT_14 || version2 === Version.DRAFT_15 || version2 === Version.DRAFT_16) {
      if (this.requestId === void 0) throw new Error("requestId required for draft14-16");
      await w.u62(this.requestId);
    }
    await this.parameters.encode(w, version2);
  }
  async encode(w, version2) {
    return encode6(w, (wr) => this.#encode(wr, version2));
  }
  static async #decode(r, version2) {
    const requestId = version2 === Version.DRAFT_14 || version2 === Version.DRAFT_15 || version2 === Version.DRAFT_16 ? await r.u62() : void 0;
    const parameters = await Parameters.decode(r, version2);
    await decode7(r, version2);
    return new _RequestOk({ requestId, parameters });
  }
  static async decode(r, version2) {
    return decode6(r, (rd) => _RequestOk.#decode(rd, version2));
  }
};
var RequestError = class _RequestError {
  static id = 5;
  requestId;
  errorCode;
  reasonPhrase;
  retryInterval;
  constructor({
    requestId,
    errorCode,
    reasonPhrase,
    retryInterval = 0n
  }) {
    this.requestId = requestId;
    this.errorCode = errorCode;
    this.reasonPhrase = reasonPhrase;
    this.retryInterval = retryInterval;
  }
  async #encode(w, version2) {
    if (version2 === Version.DRAFT_14 || version2 === Version.DRAFT_15 || version2 === Version.DRAFT_16) {
      if (this.requestId === void 0) throw new Error("requestId required for draft14-16");
      await w.u62(this.requestId);
    }
    await w.u62(BigInt(this.errorCode));
    if (version2 !== Version.DRAFT_14 && version2 !== Version.DRAFT_15) {
      await w.u62(this.retryInterval);
    }
    await w.string(this.reasonPhrase);
  }
  async encode(w, version2) {
    return encode6(w, (wr) => this.#encode(wr, version2));
  }
  static async #decode(r, version2) {
    const requestId = version2 === Version.DRAFT_14 || version2 === Version.DRAFT_15 || version2 === Version.DRAFT_16 ? await r.u62() : void 0;
    const errorCode = Number(await r.u62());
    const retryInterval = version2 === Version.DRAFT_14 || version2 === Version.DRAFT_15 ? 0n : await r.u62();
    const reasonPhrase = await r.string();
    return new _RequestError({ requestId, errorCode, reasonPhrase, retryInterval });
  }
  static async decode(r, version2) {
    return decode6(r, (rd) => _RequestError.#decode(rd, version2));
  }
};

// vendor/moq/js/net/src/ietf/subscribe.ts
var GROUP_ORDER = 2;
function joinFilter(version2) {
  return isDraft20(version2) ? { kind: "relative", groups: 1n } : { kind: "unfiltered" };
}
var Subscribe = class _Subscribe {
  static id = 3;
  requestId;
  trackNamespace;
  trackName;
  subscriberPriority;
  /** Which Objects the subscription delivers, resolved by the publisher against the live edge. */
  filter;
  /** The draft-20 backfill requested alongside it, or `undefined` for none. */
  fill;
  /** Whether the subscriber wants Track Properties on the response (INCLUDE_PROPERTIES). */
  propertiesWanted;
  constructor({
    requestId,
    trackNamespace,
    trackName,
    subscriberPriority,
    filter,
    fill,
    propertiesWanted
  }) {
    this.requestId = requestId;
    this.trackNamespace = trackNamespace;
    this.trackName = trackName;
    this.subscriberPriority = subscriberPriority;
    this.filter = filter ?? { kind: "unfiltered" };
    this.fill = fill;
    this.propertiesWanted = propertiesWanted ?? true;
  }
  async #encode(w, version2) {
    await w.u62(this.requestId);
    if (version2 === Version.DRAFT_17) {
      await w.u62(0n);
    }
    await encode3(w, this.trackNamespace);
    await w.string(this.trackName);
    if (version2 === Version.DRAFT_14) {
      await w.u8(this.subscriberPriority);
      await w.u8(GROUP_ORDER);
      await w.bool(true);
      await w.write(encode8(this.filter, version2));
      await w.u53(0);
    } else {
      const params = new Parameters();
      params.subscriberPriority = this.subscriberPriority;
      params.groupOrder = GROUP_ORDER;
      params.forward = true;
      params.subscriptionFilter = encode8(this.filter, version2);
      if (isDraft20(version2)) {
        if (this.fill) {
          params.fillParameters = encodeFill(this.fill, version2);
        }
        if (!this.propertiesWanted) {
          params.includeProperties = false;
        }
      }
      await params.encode(w, version2);
    }
  }
  async encode(w, version2) {
    return encode6(w, (mw) => this.#encode(mw, version2));
  }
  static async decode(r, version2) {
    return decode6(r, (mr) => _Subscribe.#decode(mr, version2));
  }
  static async #decode(r, version2) {
    const requestId = await r.u62();
    if (version2 === Version.DRAFT_17) {
      await r.u62();
    }
    const trackNamespace = await decode3(r);
    const trackName = await r.string();
    if (version2 === Version.DRAFT_14) {
      const subscriberPriority2 = await r.u8();
      let groupOrder2 = await r.u8();
      if (groupOrder2 > 2) {
        throw new Error(`unknown group order: ${groupOrder2}`);
      }
      if (groupOrder2 === 0) {
        groupOrder2 = GROUP_ORDER;
      }
      const forward2 = await r.bool();
      if (!forward2) {
        throw new Error(`unsupported forward value: ${forward2}`);
      }
      const filter2 = await decodeInline(r);
      await Parameters.decode(r, version2);
      return new _Subscribe({ requestId, trackNamespace, trackName, subscriberPriority: subscriberPriority2, filter: filter2 });
    }
    const params = await Parameters.decode(r, version2);
    const subscriberPriority = params.subscriberPriority ?? 128;
    let groupOrder = params.groupOrder ?? GROUP_ORDER;
    if (groupOrder > 2) {
      throw new Error(`unknown group order: ${groupOrder}`);
    }
    if (groupOrder === 0) {
      groupOrder = GROUP_ORDER;
    }
    const forward = params.forward ?? true;
    if (!forward) {
      throw new Error(`unsupported forward value: ${forward}`);
    }
    const draft20 = isDraft20(version2);
    if ((params.fillParameters !== void 0 || params.includeProperties !== void 0) && !draft20) {
      throw new Error("FILL_PARAMETERS and INCLUDE_PROPERTIES need draft-20");
    }
    const raw = params.subscriptionFilter;
    const filter = raw !== void 0 ? decode8(raw, version2) : { kind: "unfiltered" };
    const rawFill = params.fillParameters;
    const fill = rawFill !== void 0 ? decodeFill(rawFill, version2) : void 0;
    return new _Subscribe({
      requestId,
      trackNamespace,
      trackName,
      subscriberPriority,
      filter,
      fill,
      // Defaults to 1, so an absent parameter means the subscriber wants them.
      propertiesWanted: params.includeProperties ?? true
    });
  }
};
var SubscribeOk = class _SubscribeOk {
  static id = 4;
  requestId;
  trackAlias;
  /**
   * The largest Location in the track (LARGEST_OBJECT), which the draft requires once the
   * track has content. It is what a subscriber sizes a fill against.
   */
  largest;
  /**
   * The Track Properties to send (draft-17+): the track's Timescale and our group order.
   *
   * Empty opts the response out of properties entirely, which is what a peer that sent
   * INCLUDE_PROPERTIES=0 asked for; declaring no Timescale also opts the track out of
   * timestamps, so the subscriber times objects by arrival.
   */
  properties;
  constructor({
    requestId,
    trackAlias,
    largest,
    properties
  }) {
    this.requestId = requestId;
    this.trackAlias = trackAlias;
    this.largest = largest;
    this.properties = properties ?? {};
  }
  async #encode(w, version2) {
    if (version2 === Version.DRAFT_14 || version2 === Version.DRAFT_15 || version2 === Version.DRAFT_16) {
      if (this.requestId === void 0) throw new Error("requestId required for draft14-16");
      await w.u62(this.requestId);
    }
    await w.u62(this.trackAlias);
    if (version2 === Version.DRAFT_14) {
      await w.u62(0n);
      await w.u8(this.properties.groupOrder ?? GROUP_ORDER);
      await w.bool(this.largest !== void 0);
      if (this.largest !== void 0) {
        await w.u62(this.largest.groupId);
        await w.u62(this.largest.objectId);
      }
      await w.u53(0);
    } else {
      const params = new Parameters();
      if (version2 === Version.DRAFT_15 && this.properties.groupOrder !== void 0) {
        params.groupOrder = this.properties.groupOrder;
      }
      if (this.largest !== void 0) {
        params.largest = this.largest;
      }
      await params.encode(w, version2);
      await encode7(w, this.properties, version2);
    }
  }
  async encode(w, version2) {
    return encode6(w, (mw) => this.#encode(mw, version2));
  }
  static async decode(r, version2) {
    return decode6(r, (mr) => _SubscribeOk.#decode(mr, version2));
  }
  static async #decode(r, version2) {
    const requestId = version2 === Version.DRAFT_14 || version2 === Version.DRAFT_15 || version2 === Version.DRAFT_16 ? await r.u62() : void 0;
    const trackAlias = await r.u62();
    let largest;
    let properties = {};
    if (version2 === Version.DRAFT_14) {
      const expires = await r.u62();
      if (expires !== BigInt(0)) {
        throw new Error(`unsupported expires: ${expires}`);
      }
      await r.u8();
      const contentExists = await r.bool();
      if (contentExists) {
        const groupId = await r.u62();
        const objectId = await r.u62();
        largest = { groupId, objectId };
      }
      await Parameters.decode(r, version2);
    } else {
      largest = (await Parameters.decode(r, version2)).largest;
      properties = await decode7(r, version2);
    }
    return new _SubscribeOk({ requestId, trackAlias, largest, properties });
  }
};
var SubscribeError = class _SubscribeError {
  static id = 5;
  requestId;
  errorCode;
  reasonPhrase;
  constructor({
    requestId,
    errorCode,
    reasonPhrase
  }) {
    this.requestId = requestId;
    this.errorCode = errorCode;
    this.reasonPhrase = reasonPhrase;
  }
  async #encode(w) {
    await w.u62(this.requestId);
    await w.u62(BigInt(this.errorCode));
    await w.string(this.reasonPhrase);
  }
  async encode(w, _version) {
    return encode6(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode6(r, _SubscribeError.#decode);
  }
  static async #decode(r) {
    const requestId = await r.u62();
    const errorCode = Number(await r.u62());
    const reasonPhrase = await r.string();
    return new _SubscribeError({ requestId, errorCode, reasonPhrase });
  }
};
var SubscribeUpdate = class _SubscribeUpdate {
  static id = 2;
  requestId;
  constructor({ requestId }) {
    this.requestId = requestId;
  }
  async #encode(w, version2) {
    if (version2 === Version.DRAFT_14) {
      await w.u62(this.requestId);
      await w.u62(0n);
      await w.u62(0n);
      await w.u62(0n);
      await w.u62(0n);
      await w.u8(128);
      await w.bool(true);
      await w.u53(0);
    } else if (version2 === Version.DRAFT_15 || version2 === Version.DRAFT_16) {
      await w.u62(this.requestId);
      await w.u62(0n);
      const params = new Parameters();
      await params.encode(w, version2);
    } else {
      await w.u62(this.requestId);
      if (version2 === Version.DRAFT_17) {
        await w.u62(0n);
      }
      const params = new Parameters();
      await params.encode(w, version2);
    }
  }
  async encode(w, version2) {
    return encode6(w, (mw) => this.#encode(mw, version2));
  }
  static async decode(r, version2) {
    return decode6(r, (mr) => _SubscribeUpdate.#decode(mr, version2));
  }
  static async #decode(r, version2) {
    if (version2 === Version.DRAFT_14) {
      const requestId = await r.u62();
      await r.u62();
      await r.u62();
      await r.u62();
      await r.u62();
      await r.u8();
      await r.bool();
      await Parameters.decode(r, version2);
      return new _SubscribeUpdate({ requestId });
    } else if (version2 === Version.DRAFT_15 || version2 === Version.DRAFT_16) {
      const requestId = await r.u62();
      await r.u62();
      await Parameters.decode(r, version2);
      return new _SubscribeUpdate({ requestId });
    } else {
      const requestId = await r.u62();
      if (version2 === Version.DRAFT_17) {
        await r.u62();
      }
      await Parameters.decode(r, version2);
      return new _SubscribeUpdate({ requestId });
    }
  }
};
var Unsubscribe = class _Unsubscribe {
  static id = 10;
  requestId;
  constructor({ requestId }) {
    this.requestId = requestId;
  }
  async #encode(w) {
    await w.u62(this.requestId);
  }
  async encode(w, _version) {
    return encode6(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode6(r, _Unsubscribe.#decode);
  }
  static async #decode(r) {
    const requestId = await r.u62();
    return new _Unsubscribe({ requestId });
  }
};

// vendor/moq/js/net/src/ietf/subscribe_namespace.ts
function isLegacyVersion(version2) {
  switch (version2) {
    case Version.DRAFT_14:
    case Version.DRAFT_15:
    case Version.DRAFT_16:
    case Version.DRAFT_17:
      return true;
    default:
      return false;
  }
}
var SubscribeNamespace = class _SubscribeNamespace {
  static id = 80;
  namespace;
  requestId;
  constructor({ namespace, requestId }) {
    this.namespace = namespace;
    this.requestId = requestId;
  }
  async #encode(w, version2) {
    if (isLegacyVersion(version2)) {
      throw new Error(`SUBSCRIBE_NAMESPACE (0x50) is draft-18+ only, not ${version2}`);
    }
    await w.u62(this.requestId);
    await encode3(w, this.namespace);
    await new Parameters().encode(w, version2);
  }
  async encode(w, version2) {
    return encode6(w, (wr) => this.#encode(wr, version2));
  }
  static async decode(r, version2) {
    return decode6(r, (rd) => _SubscribeNamespace.#decode(rd, version2));
  }
  static async #decode(r, version2) {
    if (isLegacyVersion(version2)) {
      throw new Error(`SUBSCRIBE_NAMESPACE (0x50) is draft-18+ only, not ${version2}`);
    }
    const requestId = await r.u62();
    const namespace = await decode3(r);
    await Parameters.decode(r, version2);
    return new _SubscribeNamespace({ namespace, requestId });
  }
};
var SubscribeNamespaceLegacy = class _SubscribeNamespaceLegacy {
  static id = 17;
  namespace;
  requestId;
  subscribeOptions;
  // v16/v17: default 0x01 (NAMESPACE only)
  constructor({
    namespace,
    requestId,
    subscribeOptions = 1
  }) {
    this.namespace = namespace;
    this.requestId = requestId;
    this.subscribeOptions = subscribeOptions;
  }
  async #encode(w, version2) {
    if (!isLegacyVersion(version2)) {
      throw new Error(`legacy SUBSCRIBE_NAMESPACE (0x11) is draft-14..17 only, not ${version2}`);
    }
    await w.u62(this.requestId);
    if (version2 === Version.DRAFT_17) {
      await w.u62(0n);
    }
    await encode3(w, this.namespace);
    if (version2 === Version.DRAFT_16 || version2 === Version.DRAFT_17) {
      await w.u53(this.subscribeOptions);
    }
    await new Parameters().encode(w, version2);
  }
  async encode(w, version2) {
    return encode6(w, (wr) => this.#encode(wr, version2));
  }
  static async decode(r, version2) {
    return decode6(r, (rd) => _SubscribeNamespaceLegacy.#decode(rd, version2));
  }
  static async #decode(r, version2) {
    if (!isLegacyVersion(version2)) {
      throw new Error(`legacy SUBSCRIBE_NAMESPACE (0x11) is draft-14..17 only, not ${version2}`);
    }
    const requestId = await r.u62();
    if (version2 === Version.DRAFT_17) {
      await r.u62();
    }
    const namespace = await decode3(r);
    let subscribeOptions = 1;
    if (version2 === Version.DRAFT_16 || version2 === Version.DRAFT_17) {
      subscribeOptions = await r.u53();
    }
    await Parameters.decode(r, version2);
    return new _SubscribeNamespaceLegacy({ namespace, requestId, subscribeOptions });
  }
};
var SubscribeNamespaceOk = class _SubscribeNamespaceOk {
  static id = 18;
  requestId;
  constructor({ requestId }) {
    this.requestId = requestId;
  }
  async #encode(w) {
    await w.u62(this.requestId);
  }
  async encode(w, _version) {
    return encode6(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode6(r, _SubscribeNamespaceOk.#decode);
  }
  static async #decode(r) {
    const requestId = await r.u62();
    return new _SubscribeNamespaceOk({ requestId });
  }
};
var UnsubscribeNamespace = class _UnsubscribeNamespace {
  static id = 20;
  requestId;
  constructor({ requestId }) {
    this.requestId = requestId;
  }
  async #encode(w) {
    await w.u62(this.requestId);
  }
  async encode(w, _version) {
    return encode6(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode6(r, _UnsubscribeNamespace.#decode);
  }
  static async #decode(r) {
    const requestId = await r.u62();
    return new _UnsubscribeNamespace({ requestId });
  }
};
var SubscribeNamespaceEntry = class _SubscribeNamespaceEntry {
  static id = 8;
  suffix;
  /**
   * The MoQ Cluster parameters. Set selects the extended form, `undefined` the base one.
   * An endpoint must not append them on a session that did not negotiate.
   */
  cluster;
  constructor({ suffix, cluster }) {
    this.suffix = suffix;
    this.cluster = cluster;
  }
  async #encode(w, version2) {
    await encode3(w, this.suffix);
    if (this.cluster) await intoParams(this.cluster).encode(w, version2);
  }
  async encode(w, version2) {
    return encode6(w, (wr) => this.#encode(wr, version2));
  }
  /**
   * Decode the message, expecting the extended form when the session negotiated the MoQ
   * Cluster extension. See {@link PublishNamespace.decode}.
   */
  static async decode(r, version2, negotiated2 = false) {
    return decode6(r, (rd) => _SubscribeNamespaceEntry.#decode(rd, version2, negotiated2));
  }
  static async #decode(r, version2, negotiated2) {
    const suffix = await decode3(r);
    if (!negotiated2) return new _SubscribeNamespaceEntry({ suffix });
    return new _SubscribeNamespaceEntry({ suffix, cluster: await decodeParams(r, version2) });
  }
};
var SubscribeNamespaceEntryDone = class _SubscribeNamespaceEntryDone {
  static id = 14;
  suffix;
  constructor({ suffix }) {
    this.suffix = suffix;
  }
  async #encode(w) {
    await encode3(w, this.suffix);
  }
  async encode(w, _version) {
    return encode6(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode6(r, _SubscribeNamespaceEntryDone.#decode);
  }
  static async #decode(r) {
    const suffix = await decode3(r);
    return new _SubscribeNamespaceEntryDone({ suffix });
  }
};
var PublishBlocked = class _PublishBlocked {
  static id = 15;
  suffix;
  trackName;
  constructor({ suffix, trackName }) {
    this.suffix = suffix;
    this.trackName = trackName;
  }
  async #encode(w) {
    await encode3(w, this.suffix);
    await w.string(this.trackName);
  }
  async encode(w, _version) {
    return encode6(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode6(r, _PublishBlocked.#decode);
  }
  static async #decode(r) {
    const suffix = await decode3(r);
    const trackName = await r.string();
    return new _PublishBlocked({ suffix, trackName });
  }
};

// vendor/moq/js/net/src/ietf/track.ts
var GROUP_ORDER2 = 2;
var TrackStatusRequest = class _TrackStatusRequest {
  static id = 13;
  requestId;
  trackNamespace;
  trackName;
  constructor({
    requestId,
    trackNamespace,
    trackName
  }) {
    this.requestId = requestId;
    this.trackNamespace = trackNamespace;
    this.trackName = trackName;
  }
  async #encode(w, version2) {
    await w.u62(this.requestId);
    if (version2 === Version.DRAFT_17) {
      await w.u62(0n);
    }
    await encode3(w, this.trackNamespace);
    await w.string(this.trackName);
    if (version2 === Version.DRAFT_14) {
      await w.u8(0);
      await w.u8(GROUP_ORDER2);
      await w.bool(false);
      await w.u53(2);
      await w.u53(0);
    } else {
      const params = new Parameters();
      await params.encode(w, version2);
    }
  }
  async encode(w, version2) {
    return encode6(w, (mw) => this.#encode(mw, version2));
  }
  static async decode(r, version2) {
    return decode6(r, (mr) => _TrackStatusRequest.#decode(mr, version2));
  }
  static async #decode(r, version2) {
    const requestId = await r.u62();
    if (version2 === Version.DRAFT_17) {
      await r.u62();
    }
    const trackNamespace = await decode3(r);
    const trackName = await r.string();
    if (version2 === Version.DRAFT_14) {
      await r.u8();
      await r.u8();
      await r.bool();
      await r.u53();
      await Parameters.decode(r, version2);
    } else {
      await Parameters.decode(r, version2);
    }
    return new _TrackStatusRequest({ requestId, trackNamespace, trackName });
  }
};
var TrackStatus = class _TrackStatus {
  static id = 14;
  trackNamespace;
  trackName;
  statusCode;
  lastGroupId;
  lastObjectId;
  constructor({
    trackNamespace,
    trackName,
    statusCode,
    lastGroupId,
    lastObjectId
  }) {
    this.trackNamespace = trackNamespace;
    this.trackName = trackName;
    this.statusCode = statusCode;
    this.lastGroupId = lastGroupId;
    this.lastObjectId = lastObjectId;
  }
  async #encode(w) {
    await encode3(w, this.trackNamespace);
    await w.string(this.trackName);
    await w.u62(BigInt(this.statusCode));
    await w.u62(this.lastGroupId);
    await w.u62(this.lastObjectId);
  }
  async encode(w, _version) {
    return encode6(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode6(r, _TrackStatus.#decode);
  }
  static async #decode(r) {
    const trackNamespace = await decode3(r);
    const trackName = await r.string();
    const statusCode = Number(await r.u62());
    const lastGroupId = await r.u62();
    const lastObjectId = await r.u62();
    return new _TrackStatus({ trackNamespace, trackName, statusCode, lastGroupId, lastObjectId });
  }
  // Track status codes
  static STATUS_IN_PROGRESS = 0;
  static STATUS_NOT_FOUND = 1;
  static STATUS_NOT_AUTHORIZED = 2;
  static STATUS_ENDED = 3;
};

// vendor/moq/js/net/src/ietf/publisher.ts
var RETRY_BASE = 100;
var RETRY_MAX = 5e3;
var PUBLISH_DONE_STATUS = {
  INTERNAL_ERROR: 0,
  TRACK_ENDED: 2
};
var ADVERTISE_TIMEOUT_MS = 5e3;
function retryAfter(delay) {
  return new Promise((resolve2) => setTimeout(resolve2, delay * (0.5 + Math.random() / 2)));
}
var Publisher = class {
  #quic;
  #session;
  #requiresSolicitation;
  // Local patch: the single writer for outgoing OBJECT_DATAGRAMs (getWriter locks the
  // stream), acquired lazily on the first datagram a subscribed track produces.
  #datagramWriter;
  // What every advertisement carries on a session that negotiated the MoQ Cluster
  // extension: a hop path holding our own id, so the peer can tell that what it hears
  // back came from us. `undefined` when nothing negotiated it.
  #advert;
  // Our published broadcasts.
  // It's a signal so we can live update any subscribe_namespace streams.
  #broadcasts = new Signal(/* @__PURE__ */ new Map());
  /**
   * Creates a new Publisher instance.
   *
   * @internal
   */
  constructor({
    quic,
    session,
    requiresSolicitation,
    cluster
  }) {
    this.#quic = quic;
    this.#session = session;
    this.#requiresSolicitation = requiresSolicitation;
    this.#advert = advertise(cluster);
  }
  /**
   * Publishes a broadcast with any associated tracks.
   * The namespace is advertised with an unsolicited PUBLISH_NAMESPACE, or on request
   * if the peer asked for that (see {@link runPublishNamespaces}).
   */
  publish(path, broadcast) {
    this.#broadcasts.mutate((broadcasts) => {
      if (!broadcasts) throw new Error("closed");
      broadcasts.set(path, broadcast);
    });
    void broadcast.closed.then(() => {
      this.#broadcasts.mutate((broadcasts) => {
        if (broadcasts?.get(path) === broadcast) {
          broadcasts.delete(path);
        }
      });
    });
  }
  /**
   * Handles an incoming SUBSCRIBE request on a bidi stream.
   * Owns the full lifecycle: sends response, serves track data, waits for close.
   *
   * @internal
   */
  async runSubscribe(msg, stream) {
    const version2 = this.#session.version;
    const name = msg.trackNamespace;
    const broadcast = this.#broadcasts.peek()?.get(name);
    if (!broadcast) {
      if (version2 === Version.DRAFT_14) {
        await stream.writer.u53(SubscribeError.id);
        const err = new SubscribeError({
          requestId: msg.requestId,
          errorCode: 404,
          reasonPhrase: "Broadcast not found"
        });
        await err.encode(stream.writer, version2);
      } else {
        await stream.writer.u53(RequestError.id);
        const err = new RequestError({
          requestId: version2 === Version.DRAFT_15 || version2 === Version.DRAFT_16 ? msg.requestId : void 0,
          errorCode: 404,
          reasonPhrase: "Broadcast not found"
        });
        await err.encode(stream.writer, version2);
      }
      stream.close();
      return;
    }
    const priority = fromWire(msg.subscriberPriority);
    const track = broadcast.subscribe(msg.trackName, { priority });
    let cache;
    try {
      const timescale = (await track.info()).timescale;
      const edge = liveEdge(track);
      const range = subscribeRange(msg, edge, version2);
      track.update({
        priority,
        startGroup: range.start && Number(range.start.group),
        endGroup: range.end && Number(range.end.group)
      });
      const startGroup = range.start ? Number(range.start.group) : track.latest();
      if (startGroup !== void 0) track.startAt(startGroup);
      const fill = msg.fill && isDraft20(version2) ? fillRange(msg.fill, msg.filter, edge.largest) : void 0;
      cache = fill && fill.kind !== "empty" ? track.fork({ priority }) : void 0;
      await stream.writer.u53(SubscribeOk.id);
      const ok = new SubscribeOk({
        requestId: version2 === Version.DRAFT_14 || version2 === Version.DRAFT_15 || version2 === Version.DRAFT_16 ? msg.requestId : void 0,
        trackAlias: msg.requestId,
        // Required once the track has content; a fill-requesting subscriber sizes its
        // backfill against this. Below draft-20 round it down to the start of its
        // group, matching what the subscription serves there: a draft-20 subscriber
        // recovers a group's head with a FILL, which we serve, while older drafts
        // recover it with a joining FETCH, which we do not, so naming a mid-group
        // start would strand them.
        largest: edge.largest && (isDraft20(version2) ? { groupId: edge.largest.group, objectId: edge.largest.object } : { groupId: edge.largest.group, objectId: 0n }),
        properties: msg.propertiesWanted ? (
          // Declaring the timescale is what opts the track into timestamps; every
          // object Timestamp below is in these units. We serve the newest group
          // first, matching moq-lite.
          { timescale, groupOrder: DESCENDING }
        ) : (
          // INCLUDE_PROPERTIES=0. The block stays present but empty, which also means
          // the track opts out of timestamps for this subscriber.
          {}
        )
      });
      await ok.encode(stream.writer, version2);
      console.debug(`publish ok: broadcast=${name} track=${track.name}`);
      let finished = false;
      let unsubscribe;
      const unsubscribed = new Promise((resolve2) => {
        unsubscribe = resolve2;
      });
      void stream.reader.closed.then(
        () => {
          if (!finished) unsubscribe();
        },
        // A reset is always the peer.
        () => unsubscribe()
      );
      const serving = (async () => {
        for (; ; ) {
          const group = await track.recvGroup();
          if (!group) return;
          if (range.end !== void 0 && BigInt(group.sequence) > range.end.group) {
            group.close();
            continue;
          }
          void this.#runGroup({
            requestId: msg.requestId,
            group,
            timescale,
            stamped: msg.propertiesWanted,
            slice: groupSlice(range, group.sequence),
            unsubscribed
          });
        }
      })();
      void this.#runDatagrams(msg.requestId, track);
      const filling = fill && cache ? this.#runFill({
        requestId: msg.requestId,
        priority,
        fill,
        cache,
        timescale,
        stamped: msg.propertiesWanted,
        unsubscribed
      }) : Promise.resolve();
      let publishError;
      try {
        await Promise.race([Promise.all([serving, filling]), stream.reader.closed]);
      } catch (err) {
        publishError = error(err);
      }
      console.debug(`publish done: broadcast=${name} track=${track.name}`);
      if (publishError) {
        console.warn(`publish error: broadcast=${name} track=${track.name} error=${reason(publishError)}`);
      }
      try {
        await stream.writer.u53(PublishDone.id);
        const done = new PublishDone({
          requestId: version2 === Version.DRAFT_14 || version2 === Version.DRAFT_15 || version2 === Version.DRAFT_16 ? msg.requestId : void 0,
          statusCode: publishError ? PUBLISH_DONE_STATUS.INTERNAL_ERROR : PUBLISH_DONE_STATUS.TRACK_ENDED,
          reasonPhrase: publishError ? "internal error" : "track ended"
        });
        await done.encode(stream.writer, version2);
      } catch {
      }
      finished = true;
      stream.close();
    } catch (err) {
      const e = error(err);
      console.warn(`publish error: broadcast=${name} track=${track.name} error=${reason(e)}`);
      stream.abort(e);
    } finally {
      track.close();
      cache?.close();
    }
  }
  /**
   * Local patch: forward a subscribed track's datagrams as Draft 21 OBJECT_DATAGRAMs
   * (Group ID = datagram sequence, Object ID 0, END_OF_GROUP). Oversize datagrams and
   * send failures drop that datagram only; the loop ends when the track closes.
   */
  async #runDatagrams(requestId, track) {
    try {
      for (; ; ) {
        const datagram = await track.recvDatagram();
        if (!datagram) return;
        if (this.#datagramWriter === void 0) {
          this.#datagramWriter = datagramWriter(this.#quic) ?? null;
        }
        const writer = this.#datagramWriter;
        if (!writer) continue;
        const body = encodeObjectDatagram(requestId, datagram.sequence, datagram.payload, this.#session.version);
        const max = maxDatagramSize(this.#quic);
        if (max > 0 && body.byteLength > max) {
          console.debug(`dropping oversize object datagram: size=${body.byteLength} max=${max}`);
          continue;
        }
        writer.write(body).catch(() => void 0);
      }
    } catch (err) {
      console.debug(`datagram forwarding stopped: ${reason(err)}`);
    }
  }
  /**
   * Runs a group and sends its frames using ObjectStream (Subgroup delivery mode).
   */
  async #runGroup(options) {
    const { requestId, group, timescale, stamped, slice, unsubscribed } = options;
    try {
      const stream = await Writer.tryOpen(this.#quic, {
        cancel: unsubscribed,
        version: this.#session.version,
        waitUntilAvailable: false
      });
      if (!stream) {
        group.close(new Error("no stream slot"));
        return;
      }
      const header = new Group({
        trackAlias: requestId,
        groupId: group.sequence,
        subGroupId: 0,
        publisherPriority: 0,
        flags: {
          // The object properties carry the timestamp, so there is nothing to write
          // when the track declared no units to read one in.
          hasExtensions: stamped,
          hasSubgroup: false,
          hasSubgroupObject: false,
          hasEnd: true,
          hasPriority: true,
          // Only honest when the stream really starts at the group's first object;
          // a trimmed head starts partway through.
          firstObject: slice.skip === 0
        }
      });
      await header.encode(stream, this.#session.version);
      try {
        let first = true;
        let next = slice.skip;
        for (; ; ) {
          if (slice.until !== void 0 && next >= slice.until) break;
          const frame = await Promise.race([group.readFrameSequence({ from: slice.skip }), stream.closed]);
          if (!frame) break;
          next = frame.sequence + 1;
          if (slice.until !== void 0 && frame.sequence >= slice.until) break;
          const obj = new Frame({ payload: frame.payload, timestamp: frame.timestamp });
          const delta = first ? frame.sequence : 0;
          first = false;
          await obj.encode(stream, header.flags, timescale, this.#session.version, delta);
        }
        stream.close();
      } catch (err) {
        stream.reset(error(err));
      }
    } finally {
      group.close();
    }
  }
  /**
   * Serve a draft-20 fill on its own fetch stream: the requested range, read from the
   * group cache, capped at the Largest Object snapshot.
   *
   * A fill is a promise once requested. An empty range opens no stream, but a range we
   * cannot serve still opens one and resets it right after the FETCH_HEADER, the draft's
   * fill-failure signal. Nothing here touches the subscription either way.
   */
  async #runFill(options) {
    const { requestId, fill, cache, timescale, stamped, unsubscribed } = options;
    const version2 = this.#session.version;
    let stream;
    try {
      if (fill.kind === "empty") return;
      stream = await Writer.tryOpen(this.#quic, { cancel: unsubscribed, version: version2 });
      if (!stream) {
        console.debug(`fill stream failed to open: fill=${requestId}`);
        return;
      }
      await stream.u53(FetchHeader.type);
      await new FetchHeader({ requestId }).encode(stream, version2);
      if (fill.kind !== "group") {
        throw new Error("a fill spanning several groups is not supported");
      }
      const group = takeGroup(cache, Number(fill.sequence));
      try {
        await this.#writeFillGroup(stream, group, fill, timescale, stamped, unsubscribed);
      } finally {
        group.close();
      }
      stream.close();
      console.debug(`fill complete: fill=${requestId}`);
    } catch (err) {
      const e = error(err);
      console.debug(`fill failed, resetting its stream: fill=${requestId} error=${reason(e)}`);
      stream?.reset(e);
    } finally {
      cache.close();
    }
  }
  /**
   * Write one cached group's frames as draft-20 fetch objects.
   *
   * The cap is the Largest Object snapshot: the group may keep growing, but everything
   * past the snapshot belongs to the subscription, not the fill.
   */
  async #writeFillGroup(stream, group, fill, timescale, stamped, unsubscribed) {
    let first = true;
    let next = 0n;
    let left = false;
    const cancelled = unsubscribed.then(() => {
      left = true;
      return void 0;
    });
    for (; ; ) {
      if (fill.until !== void 0 && next >= fill.until) break;
      const frame = await Promise.race([
        group.readFrameSequence({ from: Number(fill.skip) }),
        stream.closed,
        cancelled
      ]);
      if (left) throw new Error("unsubscribed before the fill finished");
      if (!frame) break;
      next = BigInt(frame.sequence) + 1n;
      if (fill.until !== void 0 && BigInt(frame.sequence) >= fill.until) break;
      const obj = new FetchFrame({ payload: frame.payload, timestamp: stamped ? frame.timestamp : void 0 });
      await obj.encode(
        stream,
        { group: Number(fill.sequence), object: frame.sequence, first },
        timescale,
        this.#session.version
      );
      first = false;
    }
  }
  /**
   * Handles an incoming SUBSCRIBE_NAMESPACE on a bidi stream.
   *
   * This carries the advertisements only when the peer asked to be told on request
   * (MoQ Solicit); otherwise {@link runPublishNamespaces} has already announced
   * everything and repeating it here would leave the peer holding two sources for one
   * broadcast. Draft-16+ streams Namespace entries inline; draft-14/15 predate those
   * messages, so each advertisement is a PUBLISH_NAMESPACE request of its own.
   *
   * @internal
   */
  async runSubscribeNamespace(msg, stream) {
    const version2 = this.#session.version;
    const prefix = msg.namespace;
    const legacy = version2 === Version.DRAFT_14 || version2 === Version.DRAFT_15;
    const requests = /* @__PURE__ */ new Map();
    try {
      if (version2 === Version.DRAFT_14) {
        await stream.writer.u53(SubscribeNamespaceOk.id);
        const ok = new SubscribeNamespaceOk({ requestId: msg.requestId });
        await ok.encode(stream.writer, version2);
      } else {
        await stream.writer.u53(RequestOk.id);
        const ok = new RequestOk({
          requestId: version2 === Version.DRAFT_15 || version2 === Version.DRAFT_16 ? msg.requestId : void 0
        });
        await ok.encode(stream.writer, version2);
      }
      if (!this.#requiresSolicitation) {
        await stream.reader.closed;
        stream.close();
        return;
      }
      const advertise2 = async (suffix) => {
        if (legacy) {
          return await this.#advertise(join(prefix, suffix), requests, refused);
        }
        await stream.writer.u53(SubscribeNamespaceEntry.id);
        await new SubscribeNamespaceEntry({ suffix, cluster: this.#advert }).encode(stream.writer, version2);
        return true;
      };
      const withdraw = async (suffix) => {
        if (legacy) {
          await this.#withdraw(join(prefix, suffix), requests);
        } else {
          await stream.writer.u53(SubscribeNamespaceEntryDone.id);
          await new SubscribeNamespaceEntryDone({ suffix }).encode(stream.writer, version2);
        }
      };
      let active = /* @__PURE__ */ new Map();
      let retry = 0;
      const refused = /* @__PURE__ */ new Map();
      for (; ; ) {
        let dispose;
        const changed = new Promise((resolve2) => {
          dispose = this.#broadcasts.changed(resolve2);
        });
        const broadcasts = this.#broadcasts.peek();
        if (!broadcasts) {
          dispose();
          break;
        }
        const updated = /* @__PURE__ */ new Map();
        for (const [name, producer] of broadcasts) {
          const suffix = stripPrefix(prefix, name);
          if (suffix === null) continue;
          updated.set(suffix, producer);
        }
        const live = new Set([...updated.keys()].map((suffix) => join(prefix, suffix)));
        for (const path of [...refused.keys()]) {
          if (!live.has(path)) refused.delete(path);
        }
        const held = new Map(active);
        for (const [suffix, producer] of active) {
          if (updated.get(suffix) === producer) continue;
          await withdraw(suffix);
          held.delete(suffix);
        }
        for (const [suffix, producer] of updated) {
          if (active.get(suffix) === producer) continue;
          if (this.#offerable(join(prefix, suffix), refused)) {
            if (await advertise2(suffix)) held.set(suffix, producer);
          }
        }
        active = held;
        const outstanding = [...updated].some(
          ([suffix, producer]) => active.get(suffix) !== producer && this.#pending(join(prefix, suffix), refused)
        );
        retry = outstanding ? Math.min(retry ? retry * 2 : RETRY_BASE, RETRY_MAX) : 0;
        const next = await (retry ? Promise.race([changed, stream.reader.closed, retryAfter(retry).then(() => broadcasts)]) : Promise.race([changed, stream.reader.closed]));
        dispose();
        if (!next) break;
      }
      stream.close();
    } catch (err) {
      const e = error(err);
      console.debug(`subscribe_namespace stream error: ${reason(e)}`);
      stream.abort(e);
    } finally {
      for (const path of [...requests.keys()]) {
        await this.#withdraw(path, requests);
      }
    }
  }
  /**
   * Advertise every published broadcast with an unsolicited PUBLISH_NAMESPACE, until
   * the publisher is closed.
   *
   * The peers that never send SUBSCRIBE_NAMESPACE are exactly the ones expecting a
   * publisher to announce itself, so announcing is the default. A peer that would
   * rather ask says so in its SETUP (MoQ Solicit) and this does nothing, leaving
   * {@link runSubscribeNamespace} to carry the advertisements instead. Exactly one of
   * the two is live, so the peer never hears a namespace twice.
   *
   * @internal
   */
  async runPublishNamespaces() {
    if (this.#requiresSolicitation) {
      return;
    }
    const requests = /* @__PURE__ */ new Map();
    let dispose;
    try {
      let active = /* @__PURE__ */ new Map();
      let retry = 0;
      const refused = /* @__PURE__ */ new Map();
      for (; ; ) {
        const changed = new Promise((resolve2) => {
          dispose = this.#broadcasts.changed(resolve2);
        });
        const broadcasts = this.#broadcasts.peek();
        if (!broadcasts) {
          dispose?.();
          break;
        }
        const updated = new Map(broadcasts);
        for (const path of [...refused.keys()]) {
          if (!updated.has(path)) refused.delete(path);
        }
        for (const [path, producer] of active) {
          if (updated.get(path) === producer) continue;
          await this.#withdraw(path, requests);
        }
        for (const [path, producer] of updated) {
          if (active.get(path) === producer) continue;
          if (this.#offerable(path, refused)) {
            await this.#advertise(path, requests, refused);
          }
        }
        const held = /* @__PURE__ */ new Map();
        for (const path of requests.keys()) {
          const producer = updated.get(path);
          if (producer) held.set(path, producer);
        }
        active = held;
        const outstanding = [...updated].some(
          ([path, producer]) => active.get(path) !== producer && this.#pending(path, refused)
        );
        retry = outstanding ? Math.min(retry ? retry * 2 : RETRY_BASE, RETRY_MAX) : 0;
        const next = await (retry ? Promise.race([changed, retryAfter(retry).then(() => broadcasts)]) : changed);
        dispose?.();
        if (!next) break;
      }
    } catch (err) {
      console.warn(`publish_namespace loop failed: ${reason(error(err))}`);
    } finally {
      dispose?.();
      for (const path of [...requests.keys()]) {
        await this.#withdraw(path, requests);
      }
    }
  }
  /**
   * Whether a namespace may be offered to the peer right now.
   *
   * A peer that asked never to be offered it again means it, whatever brought us back;
   * one that named a minimum wait gets it, even when our own backoff comes round sooner.
   */
  #offerable(path, refused) {
    const entry = refused.get(path);
    if (entry === void 0) return true;
    return entry !== "never" && Date.now() >= entry;
  }
  /**
   * Whether the loop should keep coming back to a namespace the peer does not hold.
   *
   * Distinct from {@link offerable}, and the difference is what arms the retry: a
   * namespace waiting out a minimum is not offerable yet but is still pending, and
   * gating the timer on offerable instead would disarm it for exactly the wait it is
   * supposed to be counting. Only a refusal that forbids retrying ends it.
   */
  #pending(path, refused) {
    return refused.get(path) !== "never";
  }
  /**
   * Advertise one namespace on its own PUBLISH_NAMESPACE request. A declined request
   * is logged and skipped: a peer that wants none of this rejects each one and stays
   * connected.
   *
   * `refused` records what a refusal said about coming back, so a peer that asked not to
   * be offered a namespace again is not re-offered it by the retry above.
   */
  async #advertise(path, requests, refused) {
    const requestId = await this.#session.nextRequestId();
    if (requestId === void 0) return false;
    let request;
    try {
      request = await this.#session.openBi();
      const stream = request;
      await withTimeout(
        (async () => {
          await stream.writer.u53(PublishNamespace.id);
          const msg = new PublishNamespace({ requestId, trackNamespace: path, cluster: this.#advert });
          await msg.encode(stream.writer, this.#session.version);
          const respTypeId = await stream.reader.u53();
          if (respTypeId === RequestError.id) {
            const err = await RequestError.decode(stream.reader, this.#session.version);
            const legacy = this.#session.version === Version.DRAFT_14 || this.#session.version === Version.DRAFT_15;
            if (!legacy) {
              refused.set(
                path,
                err.retryInterval === 0n ? "never" : Date.now() + Number(err.retryInterval)
              );
            }
            throw new Error(`PublishNamespace rejected: ${err.errorCode} ${err.reasonPhrase}`);
          }
          if (respTypeId !== RequestOk.id) {
            throw new Error(`PublishNamespace rejected: typeId=0x${respTypeId.toString(16)}`);
          }
          if (this.#session.version === Version.DRAFT_14) {
            await PublishNamespaceOk.decode(stream.reader, this.#session.version);
          } else {
            await RequestOk.decode(stream.reader, this.#session.version);
          }
        })(),
        ADVERTISE_TIMEOUT_MS,
        `advertisement timed out after ${ADVERTISE_TIMEOUT_MS}ms waiting for the peer's answer`
      );
      requests.set(path, { path, requestId, stream: request });
      return true;
    } catch (err) {
      const e = error(err);
      console.warn(`announce failed: broadcast=${path} error=${reason(e)}`);
      request?.abort(e);
      return false;
    }
  }
  /**
   * Close out a namespace's PUBLISH_NAMESPACE request with PUBLISH_NAMESPACE_DONE.
   */
  async #withdraw(path, requests) {
    const request = requests.get(path);
    if (!request) return;
    requests.delete(path);
    const version2 = this.#session.version;
    if (version2 === Version.DRAFT_14 || version2 === Version.DRAFT_15 || version2 === Version.DRAFT_16) {
      try {
        await request.stream.writer.u53(PublishNamespaceDone.id);
        const done = new PublishNamespaceDone({ trackNamespace: request.path, requestId: request.requestId });
        await done.encode(request.stream.writer, version2);
      } catch {
      }
      request.stream.close();
      return;
    }
    request.stream.close();
    await request.stream.writer.closed;
  }
  /**
   * Handles an incoming TRACK_STATUS_REQUEST on a bidi stream.
   *
   * @internal
   */
  async runTrackStatusRequest(msg, stream) {
    const version2 = this.#session.version;
    if (version2 === Version.DRAFT_14) {
      await stream.writer.u53(TrackStatus.id);
      const status = new TrackStatus({
        trackNamespace: msg.trackNamespace,
        trackName: msg.trackName,
        statusCode: TrackStatus.STATUS_NOT_FOUND,
        lastGroupId: 0n,
        lastObjectId: 0n
      });
      await status.encode(stream.writer, version2);
    } else {
      await stream.writer.u53(RequestOk.id);
      const ok = new RequestOk({
        requestId: version2 === Version.DRAFT_15 || version2 === Version.DRAFT_16 ? msg.requestId : void 0
      });
      await ok.encode(stream.writer, version2);
    }
    stream.close();
  }
  /**
   * Closes every published broadcast and stops accepting new ones.
   *
   * @internal
   */
  close() {
    try {
      this.#datagramWriter?.releaseLock();
    } catch {
    }
    this.#broadcasts.update((broadcasts) => {
      for (const broadcast of broadcasts?.values() ?? []) {
        broadcast.close();
      }
      return void 0;
    });
  }
};
function takeGroup(cache, sequence) {
  for (; ; ) {
    const group = cache.tryRecvGroup();
    if (!group) throw new Error(`group not found: ${sequence}`);
    if (group.sequence === sequence) return group;
    group.close();
    if (group.sequence > sequence) throw new Error(`group not found: ${sequence}`);
  }
}
function saturatingSub(a, b) {
  return a > b ? a - b : 0n;
}
function liveEdge(track) {
  const latest = track.latest();
  if (latest === void 0) return {};
  const largest = track.largest();
  if (!largest) return { latest: BigInt(latest), next: { group: BigInt(latest), object: 0n } };
  const group = BigInt(largest.group);
  const object = BigInt(largest.frame);
  return { latest: BigInt(latest), largest: { group, object }, next: { group, object: object + 1n } };
}
function subscribeRange(msg, edge, version2) {
  if (!isDraft20(version2)) {
    if (msg.filter.kind !== "nextObject" && msg.filter.kind !== "unfiltered") {
      console.warn(`filter not supported before draft-20, ignoring: ${msg.filter.kind}`);
    }
    return {};
  }
  return filterRange(msg.filter, edge);
}
function filterRange(filter, edge) {
  switch (filter.kind) {
    // No restriction. moq-lite starts at the beginning of the latest group, which is the
    // join point it is built around; a subscription passes objects as they are published,
    // so an absent filter is not a request to replay history.
    case "unfiltered":
      return {};
    // `{Largest.Group, Largest.Object + 1}`. Everything below it, including the already
    // published head of the current group, is outside the requested range, so the join is
    // mid-group by construction. The draft pairs this with a fill when the subscriber
    // wants the head; see {@link Publisher.runFill}.
    case "nextObject":
      return { start: edge.next };
    // `{Largest.Group + 1 - groups, 0}`: 0 is the next group and 1 is the current one.
    // Counted from `Largest.Group`, which sits below the newest group while that group has
    // no objects yet; only with no largest at all does the newest group stand in for it.
    case "relative": {
      const base = edge.largest?.group ?? edge.latest;
      if (base === void 0) return {};
      return { start: { group: saturatingSub(base + 1n, filter.groups), object: 0n } };
    }
    case "absolute":
      return {
        start: { group: filter.startGroup, object: filter.startObject },
        end: filter.endGroup === void 0 ? void 0 : { group: filter.endGroup, object: filter.endObject }
      };
  }
}
function groupSlice(range, sequence) {
  const at = BigInt(sequence);
  return {
    skip: range.start?.group === at ? Number(range.start.object) : 0,
    until: range.end?.group === at && range.end.object !== void 0 ? Number(range.end.object) + 1 : void 0
  };
}
function fillRange(fill, subscription, largest) {
  if (fill.rangeFilters) return { kind: "unsupported" };
  const filter = fill.filter ?? subscription;
  if (!largest) return { kind: "empty" };
  let start;
  switch (filter.kind) {
    // A Fetch without a filter is the whole track up to Largest Object.
    case "unfiltered":
      start = { group: 0n, object: 0n };
      break;
    // One past the edge, which for a Fetch is always empty.
    case "nextObject":
      return { kind: "empty" };
    case "relative":
      start = { group: saturatingSub(largest.group + 1n, filter.groups), object: 0n };
      break;
    case "absolute":
      start = { group: filter.startGroup, object: filter.startObject };
      break;
  }
  let end = { group: largest.group, object: largest.object };
  if (filter.kind === "absolute" && filter.endGroup !== void 0) {
    const below = filter.endGroup < largest.group || filter.endGroup === largest.group && filter.endObject !== void 0 && filter.endObject < largest.object;
    if (below) end = { group: filter.endGroup, object: filter.endObject };
  }
  if (start.group > end.group || start.group === end.group && end.object !== void 0 && end.object < start.object) {
    return { kind: "empty" };
  }
  if (start.group !== end.group) return { kind: "unsupported" };
  return {
    kind: "group",
    sequence: start.group,
    skip: start.object,
    until: end.object === void 0 ? void 0 : end.object + 1n
  };
}

// vendor/moq/js/net/src/consume.ts
var BroadcastCache = class {
  // The base handle per path; callers get reference-counted clones of it.
  #cache = /* @__PURE__ */ new Map();
  /** A shared handle to the live broadcast cached for `path`, or `undefined` on a miss. */
  get(path) {
    const base = this.#cache.get(path);
    if (base && base.closed.peek() === void 0) return base.clone();
    return void 0;
  }
  /**
   * Cache `consumer` as the base handle for `path` (evicting it once it closes) and return it.
   * Call on a {@link get} miss, after wiring up the fresh consumer's subscribe loop.
   */
  insert(path, consumer) {
    this.#cache.set(path, consumer);
    void consumer.closed.then(() => {
      if (this.#cache.get(path) === consumer) this.#cache.delete(path);
    });
    return consumer;
  }
  /**
   * Stop sharing the broadcast cached for `path`, so the next request subscribes fresh.
   *
   * Call when the path's advertisement goes away. A handle only leaves the cache on its own
   * once *every* holder has closed it, so one holder outliving the publisher (a second
   * watcher, or a caller consuming the path directly) would otherwise keep the dead
   * generation's cached tracks alive and hand them to whoever consumes the path next.
   * Existing handles are left alone: they belong to their holders, and the wire resets
   * whatever they still have open.
   *
   * Eviction is unconditional, which costs a dedup miss when two announcement streams watch
   * one path: the second stream's retraction can arrive after the first has already seen the
   * replacement, dropping the fresh entry so the next request subscribes again instead of
   * sharing. Telling that stale retraction from a live one needs a generation id on the
   * advertisement (moq-lite's `Epoch`, not yet on the wire), so until then this errs toward a
   * duplicate subscription rather than risk handing out a dead one.
   */
  evict(path) {
    this.#cache.delete(path);
  }
};

// vendor/moq/js/net/src/ietf/aliases.ts
var TRACK_ALIAS_TIMEOUT_MS = 1e3;
var RETIRED_ALIAS_CAPACITY = 64;
function sameTrack(a, b) {
  return a.broadcast === b.broadcast && a.name === b.name;
}
var RetiredTrackAlias = class extends Error {
  constructor(alias) {
    super(`track alias retired: ${alias}`);
    this.name = "RetiredTrackAlias";
  }
};
var SharedTrackAlias = class extends Error {
  constructor(alias) {
    super(`track alias shared by another subscription: ${alias}`);
    this.name = "SharedTrackAlias";
  }
};
var DuplicateTrackAlias = class extends Error {
  constructor(alias) {
    super(`duplicate track alias: ${alias}`);
    this.name = "DuplicateTrackAlias";
  }
};
var TrackAliases = class {
  #active = /* @__PURE__ */ new Map();
  #pending = /* @__PURE__ */ new Map();
  /** Aliases whose subscription we cancelled, in retirement order so the oldest is forgotten first. */
  #retired = [];
  #retiredSet = /* @__PURE__ */ new Set();
  /**
   * Waits briefly for an alias to be established by SUBSCRIBE_OK or PUBLISH.
   *
   * Throws {@link RetiredTrackAlias} at once for an alias we cancelled, rather than
   * waiting out the timeout for a binding that is never coming.
   */
  async get(alias) {
    const bound = this.#active.get(alias);
    if (bound !== void 0) return bound.value;
    if (this.#retiredSet.has(alias)) throw new RetiredTrackAlias(alias);
    const { promise, resolve: resolve2 } = Promise.withResolvers();
    let resolvers = this.#pending.get(alias);
    if (!resolvers) {
      resolvers = /* @__PURE__ */ new Set();
      this.#pending.set(alias, resolvers);
    }
    resolvers.add(resolve2);
    let timer;
    const timeout = new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`unknown track alias: ${alias}`)), TRACK_ALIAS_TIMEOUT_MS);
    });
    try {
      return await Promise.race([promise, timeout]);
    } finally {
      clearTimeout(timer);
      resolvers.delete(resolve2);
      if (this.#pending.get(alias) === resolvers && resolvers.size === 0) this.#pending.delete(alias);
    }
  }
  /**
   * Establishes an alias and releases any data streams waiting for it.
   *
   * `track` is the full track name the alias was bound to, which is what decides whether a
   * repeat is the legal sharing of an alias across subscriptions to one track
   * ({@link SharedTrackAlias}) or the collision that must fail the session
   * ({@link DuplicateTrackAlias}).
   */
  /** Local patch: the bound value for an alias right now, without waiting (datagrams). */
  peek(alias) {
    return this.#active.get(alias)?.value;
  }
  set(alias, value, track) {
    const active = this.#active.get(alias);
    if (active !== void 0) {
      if (active.value === value) return;
      throw sameTrack(active.track, track) ? new SharedTrackAlias(alias) : new DuplicateTrackAlias(alias);
    }
    this.#forget(alias);
    this.#active.set(alias, { value, track });
    const resolvers = this.#pending.get(alias);
    this.#pending.delete(alias);
    for (const resolve2 of resolvers ?? []) resolve2(value);
  }
  /**
   * Retires an alias whose subscription was cancelled, so groups still in flight for it
   * are discarded promptly instead of reported as unknown.
   *
   * Only retires an alias that still belongs to the supplied value: a later subscription
   * may already have reclaimed it, and that binding outranks a departing owner.
   *
   * Returns whether this caller still owned the binding, so metadata keyed by the alias is
   * only torn down by the owner and never out from under whoever reclaimed it.
   */
  retire(alias, value) {
    if (this.#active.get(alias)?.value !== value) return false;
    this.#active.delete(alias);
    if (this.#retiredSet.has(alias)) return true;
    this.#retiredSet.add(alias);
    this.#retired.push(alias);
    while (this.#retired.length > RETIRED_ALIAS_CAPACITY) {
      const oldest = this.#retired.shift();
      if (oldest !== void 0) this.#retiredSet.delete(oldest);
    }
    return true;
  }
  #forget(alias) {
    if (!this.#retiredSet.delete(alias)) return;
    const at = this.#retired.indexOf(alias);
    if (at !== -1) this.#retired.splice(at, 1);
  }
};

// vendor/moq/js/net/src/ietf/subscriber.ts
var SUBSCRIBE_OK_TIMEOUT_MS = 1e4;
var Subscriber2 = class {
  #session;
  // The Hop IDs this session declared; see {@link Cluster}. What the peer declared is what
  // says whether an advertisement carries a hop path, and ours is what a path looping back
  // to us contains.
  #cluster;
  // Publisher-chosen aliases used by incoming group streams.
  #aliases = new TrackAliases();
  // Units for each track's object Timestamps, from the TIMESCALE Track Property in
  // SUBSCRIBE_OK. A track missing from this map declared no timeline, so the publisher
  // opted out of timestamps and its frames are stamped on arrival instead.
  #timescales = /* @__PURE__ */ new Map();
  // Dedup consumed broadcasts per path: repeat consume() calls share one subscription.
  #consumes = new BroadcastCache();
  // Paths with a legacy PUBLISH_NAMESPACE request in flight, reserved synchronously.
  // The count below is only taken once the OK is written, and two requests that both
  // got past the duplicate check before either attached would both take one.
  #legacyRequests = /* @__PURE__ */ new Set();
  // Every announced path, counted by how many live advertisements reference it.
  //
  // A peer may advertise one namespace twice on a session: an unsolicited
  // PUBLISH_NAMESPACE and an inline NAMESPACE answering our own SUBSCRIBE_NAMESPACE are
  // two messages about one source, which the MoQ Solicit draft requires us to tolerate.
  // Counting them is what keeps the second from duplicating the announce and the first
  // to end from retracting what the other still holds.
  #announced = /* @__PURE__ */ new Map();
  // Any consumers that want each new announcement.
  #announcedConsumers = /* @__PURE__ */ new Set();
  /**
   * Creates a new Subscriber instance.
   *
   * @internal
   */
  constructor({
    session,
    cluster
  }) {
    this.#session = session;
    this.#cluster = cluster;
  }
  /**
   * Whether an advertisement is ours coming back: its hop path already ran through us, so
   * subscribing via it would route us back to ourselves.
   *
   * A conforming peer withholds these (it knows our Hop ID), so this is the backstop that
   * keeps a mesh working when one member does not. A session that negotiated nothing
   * carries no path, and there is nothing to check.
   */
  #reflected(advert) {
    return advert !== void 0 && this.#cluster !== void 0 && loops(advert, this.#cluster.self);
  }
  /**
   * Gets an announced reader for the specified prefix.
   *
   * The peer is asked with SUBSCRIBE_NAMESPACE regardless of what it declared, and an
   * unsolicited PUBLISH_NAMESPACE lands here too, so a peer that only tells and one
   * that only answers are both discovered.
   */
  announced(prefix = empty()) {
    const announced = new Producer(prefix);
    for (const active of this.#announced.keys()) {
      const suffix = stripPrefix(prefix, active);
      if (suffix === null) continue;
      announced.append({ path: suffix, active: true });
    }
    this.#announcedConsumers.add(announced);
    void this.#runAnnounced(announced, prefix).finally(() => {
      this.#announcedConsumers.delete(announced);
      announced.close();
    });
    return announced.consume();
  }
  /**
   * Record one more advertisement for a path, telling consumers only when it is the
   * first. A second one is the same namespace said twice, not news.
   */
  #attachAnnounce(path) {
    const count = this.#announced.get(path) ?? 0;
    this.#announced.set(path, count + 1);
    if (count > 0) return;
    console.debug(`announced: broadcast=${path} active=true`);
    for (const consumer of this.#announcedConsumers) {
      const suffix = stripPrefix(consumer.prefix, path);
      if (suffix === null) continue;
      consumer.append({ path: suffix, active: true });
    }
  }
  /**
   * Drop one advertisement for a path, retracting it only once the last one goes.
   */
  #detachAnnounce(path) {
    const count = this.#announced.get(path);
    if (count === void 0) return;
    if (count > 1) {
      this.#announced.set(path, count - 1);
      return;
    }
    this.#announced.delete(path);
    this.#consumes.evict(path);
    console.debug(`announced: broadcast=${path} active=false`);
    for (const consumer of this.#announcedConsumers) {
      const suffix = stripPrefix(consumer.prefix, path);
      if (suffix === null) continue;
      try {
        consumer.append({ path: suffix, active: false });
      } catch {
      }
    }
  }
  async #runAnnounced(announced, prefix) {
    const version2 = this.#session.version;
    const live = /* @__PURE__ */ new Set();
    let released = false;
    const requestId = await this.#session.nextRequestId();
    if (requestId === void 0) return;
    try {
      const stream = version2 === Version.DRAFT_16 && this.#session.openNativeBi ? await this.#session.openNativeBi() : await this.#session.openBi();
      try {
        if (version2 === Version.DRAFT_14 || version2 === Version.DRAFT_15 || version2 === Version.DRAFT_16 || version2 === Version.DRAFT_17) {
          await stream.writer.u53(SubscribeNamespaceLegacy.id);
          await new SubscribeNamespaceLegacy({ namespace: prefix, requestId }).encode(stream.writer, version2);
        } else {
          await stream.writer.u53(SubscribeNamespace.id);
          await new SubscribeNamespace({ namespace: prefix, requestId }).encode(stream.writer, version2);
        }
        console.debug(`subscribe_namespace written: requestId=${requestId}`);
        const respTypeId = await stream.reader.u53();
        if (respTypeId === RequestOk.id) {
          await RequestOk.decode(stream.reader, version2);
        } else if (respTypeId === SubscribeNamespaceOk.id) {
          const size2 = await stream.reader.u16();
          await stream.reader.read(size2);
        } else {
          throw new Error(`SubscribeNamespace rejected: typeId=0x${respTypeId.toString(16)}`);
        }
        const readLoop = (async () => {
          for (; ; ) {
            const done = await stream.reader.done();
            if (done) break;
            const msgType = await stream.reader.u53();
            if (msgType === SubscribeNamespaceEntry.id) {
              const entry = await SubscribeNamespaceEntry.decode(
                stream.reader,
                version2,
                negotiated(this.#cluster)
              );
              if (released) break;
              const path = join(prefix, entry.suffix);
              if (this.#reflected(entry.cluster)) {
                console.debug(`dropping reflected namespace: broadcast=${path}`);
                if (live.delete(path)) this.#detachAnnounce(path);
                continue;
              }
              if (!live.has(path)) {
                live.add(path);
                this.#attachAnnounce(path);
              }
            } else if (msgType === SubscribeNamespaceEntryDone.id) {
              const entry = await SubscribeNamespaceEntryDone.decode(stream.reader, version2);
              if (released) break;
              const path = join(prefix, entry.suffix);
              if (live.delete(path)) {
                this.#detachAnnounce(path);
              }
            } else if (msgType === PublishBlocked.id && version2 === Version.DRAFT_17) {
              const blocked = await PublishBlocked.decode(stream.reader, version2);
              console.debug(`publish_blocked: suffix=${blocked.suffix} track=${blocked.trackName}`);
            } else {
              throw new Error(
                `unexpected message on subscribe_namespace stream: 0x${msgType.toString(16)}`
              );
            }
          }
        })();
        readLoop.catch((err) => {
          if (err instanceof ProtocolViolation) this.#session.close();
        });
        await Promise.race([readLoop, announced.closed]);
        if (version2 === Version.DRAFT_14 || version2 === Version.DRAFT_15) {
          try {
            await stream.writer.u53(UnsubscribeNamespace.id);
            const unsub = new UnsubscribeNamespace({ requestId });
            await unsub.encode(stream.writer, version2);
          } catch {
          }
        }
        stream.close();
      } catch (err) {
        stream.abort(error(err));
        throw err;
      }
    } catch (err) {
      const e = error(err);
      console.warn(`subscribe_namespace error: ${reason(e)}`);
      if (e instanceof ProtocolViolation) this.#session.close();
      announced.close(e);
    } finally {
      released = true;
      for (const path of live) {
        this.#detachAnnounce(path);
      }
      live.clear();
    }
  }
  /**
   * Consumes a broadcast from the connection.
   *
   * Deduplicated per path: repeat calls for the same still-live path share one reference-counted
   * broadcast (and one upstream subscription). The shared broadcast closes once every caller has
   * closed its handle, so callers close normally.
   */
  consume(path) {
    return this.#consumes.get(path) ?? this.#consumes.insert(path, this.#createConsume(path));
  }
  #createConsume(path) {
    const consumer = new ConsumeBroadcast();
    void (async () => {
      for (; ; ) {
        const request = await consumer.requested();
        if (!request) break;
        void this.#runSubscribe(path, request);
      }
    })();
    return consumer;
  }
  async #runSubscribe(broadcast, request) {
    const requestId = await this.#session.nextRequestId();
    if (requestId === void 0) {
      request.reject(new Error("session closed"));
      return;
    }
    console.debug(`subscribe start: id=${requestId} broadcast=${broadcast} track=${request.name}`);
    const producer = request.accept({ ordered: false });
    const state = {};
    const setup = this.#openSubscribe(state, broadcast, request, producer, requestId);
    const waitAbandoned = async () => {
      for (; ; ) {
        await producer.unused();
        if (producer.closed.peek() !== void 0 || !producer.used.peek()) return null;
      }
    };
    let stream;
    let trackAlias;
    try {
      const result = await Promise.race([
        withTimeout(
          setup,
          SUBSCRIBE_OK_TIMEOUT_MS,
          `subscribe timed out after ${SUBSCRIBE_OK_TIMEOUT_MS}ms waiting for SUBSCRIBE_OK (browser stream limit reached?)`
        ),
        waitAbandoned()
      ]);
      if (result === null) throw new Error("subscribe abandoned before it was accepted");
      stream = result.stream;
      trackAlias = result.alias;
      console.debug(`subscribe ok: id=${requestId} broadcast=${broadcast} track=${request.name}`);
    } catch (err) {
      const e = error(err);
      producer.close(e);
      console.warn(
        `subscribe error: id=${requestId} broadcast=${broadcast} track=${request.name} error=${reason(e)}`
      );
      let torn = false;
      const cleanup = async (afterSetup) => {
        state.cancelled = true;
        if (state.registeredAlias !== void 0 && this.#aliases.retire(state.registeredAlias, producer)) {
          this.#timescales.delete(state.registeredAlias);
        }
        if (!state.stream || torn) return;
        if (!state.sent && !afterSetup) return;
        torn = true;
        if (state.sent && !state.rejected) await this.#cancelSubscribe(state.stream, requestId);
        state.stream.abort(e);
      };
      void cleanup(false);
      setup.then(
        () => cleanup(true),
        () => cleanup(true)
      );
      return;
    }
    try {
      const publisherEnded = /* @__PURE__ */ Symbol("publisher");
      const localEnded = /* @__PURE__ */ Symbol("local");
      const idle = /* @__PURE__ */ Symbol("idle");
      const done = Promise.race([
        stream.reader.closed.then(() => publisherEnded),
        producer.closed.then(() => localEnded)
      ]);
      let terminal = localEnded;
      for (; ; ) {
        const reason2 = await Promise.race([done, producer.unused().then(() => idle)]);
        if (reason2 === idle && producer.closed.peek() === void 0 && producer.used.peek()) continue;
        terminal = reason2;
        break;
      }
      if (terminal !== publisherEnded) await this.#cancelSubscribe(stream, requestId);
      producer.close();
      stream.close();
      console.debug(`subscribe close: id=${requestId} broadcast=${broadcast} track=${request.name}`);
    } catch (err) {
      const e = error(err);
      producer.close(e);
      stream.abort(e);
      console.warn(
        `subscribe error: id=${requestId} broadcast=${broadcast} track=${request.name} error=${reason(e)}`
      );
    } finally {
      if (this.#aliases.retire(trackAlias, producer)) this.#timescales.delete(trackAlias);
    }
  }
  /**
   * Tell the publisher to stop serving a subscription we are walking away from.
   *
   * v14-16 cancel with UNSUBSCRIBE (draft-16 section 9.12), which is what lets the
   * publisher destroy the subscription (section 5.1.1); v17+ removed the message and
   * rely on the stream reset instead. Every path that abandons an Established
   * subscription goes through here, because those versions carry the request over a
   * virtual stream whose reset never reaches the peer.
   */
  async #cancelSubscribe(stream, requestId) {
    const version2 = this.#session.version;
    if (version2 !== Version.DRAFT_14 && version2 !== Version.DRAFT_15 && version2 !== Version.DRAFT_16) return;
    try {
      await stream.writer.u53(Unsubscribe.id);
      await new Unsubscribe({ requestId }).encode(stream.writer, version2);
    } catch {
    }
  }
  // Opens the subscribe stream, sends SUBSCRIBE, and reads the response.
  // `state` is populated as soon as the stream opens and again when the
  // trackAlias is registered, so the caller can clean both up on timeout
  // even before this promise settles.
  async #openSubscribe(state, broadcast, request, producer, requestId) {
    const version2 = this.#session.version;
    state.stream = await this.#session.openBi();
    if (state.cancelled) throw new Error("subscribe cancelled before it was sent");
    await state.stream.writer.u53(Subscribe.id);
    const msg = new Subscribe({
      requestId,
      trackNamespace: broadcast,
      trackName: request.name,
      subscriberPriority: toWire(request.priority),
      // No fill is requested: the fill's fetch stream and the live subscription split
      // the group across two streams, and this subscriber does not reassemble them into
      // one group yet. A lenient publisher (like ours) replays the whole in-range group
      // on the subscription instead; a strict one only delivers from the next published
      // object, and that mid-group stream is dropped, degrading the join to the next
      // group boundary.
      filter: joinFilter(version2)
    });
    await msg.encode(state.stream.writer, version2);
    state.sent = true;
    if (state.cancelled) throw new Error("subscribe cancelled while it was being sent");
    console.debug(`subscribe written: id=${requestId} broadcast=${broadcast} track=${request.name}`);
    const respTypeId = await state.stream.reader.u53();
    if (respTypeId !== SubscribeOk.id) {
      let reasonPhrase = "unknown error";
      try {
        if (respTypeId === RequestError.id) {
          const err = version2 === Version.DRAFT_14 ? await SubscribeError.decode(state.stream.reader, version2) : await RequestError.decode(state.stream.reader, version2);
          reasonPhrase = `code=${err.errorCode} reason=${err.reasonPhrase}`;
        }
      } catch {
      }
      state.rejected = true;
      throw new Error(`SUBSCRIBE error: ${reasonPhrase}`);
    }
    const ok = await SubscribeOk.decode(state.stream.reader, version2);
    try {
      this.#aliases.set(ok.trackAlias, producer, { broadcast, name: request.name });
      const timescale = ok.properties.timescale;
      if (timescale !== void 0) {
        this.#timescales.set(ok.trackAlias, timescale);
      }
    } catch (err) {
      if (err instanceof DuplicateTrackAlias) this.#session.close();
      throw err;
    }
    state.registeredAlias = ok.trackAlias;
    return { stream: state.stream, alias: ok.trackAlias };
  }
  /**
   * Handles an incoming PUBLISH_NAMESPACE on a bidi stream.
   * Tracks announced broadcasts and notifies consumers.
   *
   * @internal
   */
  async runPublishNamespace(msg, stream) {
    const version2 = this.#session.version;
    const path = msg.trackNamespace;
    if (this.#reflected(msg.cluster)) {
      console.debug(`dropping reflected publish_namespace: broadcast=${path}`);
      await stream.writer.u53(RequestError.id);
      await new RequestError({
        requestId: msg.requestId,
        errorCode: 400,
        reasonPhrase: "route loops back"
      }).encode(stream.writer, version2);
      stream.close();
      return;
    }
    const legacy = version2 === Version.DRAFT_14 || version2 === Version.DRAFT_15;
    if (legacy && (this.#announced.has(path) || this.#legacyRequests.has(path))) {
      console.warn("duplicate PublishNamespace");
      if (version2 === Version.DRAFT_14) {
        await stream.writer.u53(PublishNamespaceError.id);
        await new PublishNamespaceError({
          requestId: msg.requestId,
          errorCode: 409,
          reasonPhrase: "duplicate namespace"
        }).encode(stream.writer, version2);
      } else {
        await stream.writer.u53(RequestError.id);
        await new RequestError({
          requestId: msg.requestId,
          errorCode: 409,
          reasonPhrase: "duplicate namespace"
        }).encode(stream.writer, version2);
      }
      stream.close();
      return;
    }
    if (legacy) this.#legacyRequests.add(path);
    let attached = false;
    try {
      if (version2 === Version.DRAFT_14) {
        await stream.writer.u53(PublishNamespaceOk.id);
        const ok = new PublishNamespaceOk({ requestId: msg.requestId });
        await ok.encode(stream.writer, version2);
      } else {
        await stream.writer.u53(RequestOk.id);
        const ok = new RequestOk({
          requestId: version2 === Version.DRAFT_15 || version2 === Version.DRAFT_16 ? msg.requestId : void 0
        });
        await ok.encode(stream.writer, version2);
      }
      attached = true;
      this.#attachAnnounce(path);
      const done = version2 === Version.DRAFT_16 || legacy;
      for (; ; ) {
        if (await stream.reader.done()) break;
        const typeId = await stream.reader.u53();
        if (done && typeId === PublishNamespaceDone.id) {
          await PublishNamespaceDone.decode(stream.reader, version2);
          break;
        }
        if (typeId !== PublishNamespace.id) {
          throw new ProtocolViolation(
            `unexpected message on publish_namespace stream: 0x${typeId.toString(16)}`
          );
        }
        const update = await PublishNamespace.decode(stream.reader, version2, negotiated(this.#cluster));
        if (update.requestId !== msg.requestId || update.trackNamespace !== path) {
          throw new ProtocolViolation("publish_namespace update does not match its stream");
        }
        if (this.#reflected(update.cluster)) {
          if (attached) {
            attached = false;
            console.debug(`publish_namespace now loops back, detaching: broadcast=${path}`);
            this.#detachAnnounce(path);
          }
          continue;
        }
        if (!attached) {
          attached = true;
          this.#attachAnnounce(path);
        }
      }
    } finally {
      if (legacy) this.#legacyRequests.delete(path);
      if (attached) {
        this.#detachAnnounce(path);
      }
    }
  }
  /**
   * Handles an incoming PUBLISH on a bidi stream.
   * We don't support reverse publish, so send error.
   *
   * @internal
   */
  async runPublish(msg, stream) {
    const version2 = this.#session.version;
    const NOT_SUPPORTED = 3;
    if (version2 === Version.DRAFT_14) {
      await stream.writer.u53(PublishError.id);
      const err = new PublishError({
        requestId: msg.requestId,
        errorCode: NOT_SUPPORTED,
        reasonPhrase: "publish not supported"
      });
      await err.encode(stream.writer, version2);
    } else {
      await stream.writer.u53(RequestError.id);
      const err = new RequestError({
        requestId: version2 === Version.DRAFT_15 || version2 === Version.DRAFT_16 ? msg.requestId : void 0,
        errorCode: NOT_SUPPORTED,
        reasonPhrase: "publish not supported"
      });
      await err.encode(stream.writer, version2);
    }
    stream.close();
  }
  /**
   * Handles an ObjectStream message (group + frames on uni stream).
   *
   * @internal
   */
  /**
   * Local patch: route one received OBJECT_DATAGRAM to its subscription's track as a
   * best-effort datagram. Unknown aliases, status objects and malformed datagrams are
   * dropped (a lossy datagram never costs the session).
   */
  handleDatagram(data) {
    let object;
    try {
      object = decodeObjectDatagram(data, this.#session.version);
    } catch (err) {
      console.debug(`dropping malformed object datagram: ${reason(err)}`);
      return;
    }
    if (object.payload === void 0 || object.objectId !== 0n) return;
    if (object.groupId > BigInt(Number.MAX_SAFE_INTEGER)) return;
    const track = this.#aliases.peek(object.trackAlias);
    if (!track) return;
    try {
      track.writeDatagram({ sequence: Number(object.groupId), timestamp: Timestamp.now(), payload: object.payload });
    } catch {
    }
  }
  async handleGroup(group, stream) {
    if (group.subGroupId !== 0) {
      throw new Error("subgroups are not supported");
    }
    if (!group.flags.firstObject) {
      console.debug(`dropping a group with no head: alias=${group.trackAlias} group=${group.groupId}`);
      stream.stop(new Error("a group must start at object 0"));
      return;
    }
    const producer = new Producer2(group.groupId);
    try {
      const track = await this.#aliases.get(group.trackAlias);
      track.writeGroup(producer);
      for (; ; ) {
        const done = await Promise.race([stream.done(), producer.closed, track.closed]);
        if (done !== false) break;
        const frame = await Frame.decode(
          stream,
          group.flags,
          this.#timescales.get(group.trackAlias),
          this.#session.version
        );
        if (frame.payload === void 0) break;
        producer.writeFrame({ payload: frame.payload, timestamp: frame.timestamp ?? Timestamp.now() });
      }
      producer.close();
    } catch (err) {
      const e = error(err);
      if (e instanceof RetiredTrackAlias) {
        console.debug(`dropping group for a cancelled subscription: alias=${group.trackAlias}`);
      }
      producer.close(e);
      stream.stop(e);
    }
  }
};
var ConsumeBroadcast = class _ConsumeBroadcast extends Consumer4 {
  // biome-ignore lint/complexity/noUselessConstructor: widens the protected base constructor to public
  constructor(state) {
    super(state);
  }
  // Preserve the subclass when the consume cache shares this broadcast across callers.
  clone() {
    return new _ConsumeBroadcast(this.shareState());
  }
  fetchGroup() {
    return Promise.reject(new Error("fetch group is not supported for moq-transport"));
  }
};

// vendor/moq/js/net/src/ietf/connection.ts
var Connection = class {
  // The URL of the connection.
  url;
  // The negotiated protocol version.
  version;
  // The wire transport this session runs over.
  transport;
  /** Whether the relay supports broadcast discovery; see {@link Established.discovery}. */
  discovery;
  /** moq-transport has no PROBE, so this stays empty; see {@link Established.probe}. */
  probe = new Signal({});
  // The established WebTransport session.
  #quic;
  // Session abstraction: adapter for v14-v16, native for v17.
  #session;
  // Module for contributing tracks.
  #publisher;
  // Module for distributing tracks.
  #subscriber;
  // What the peer declared about being solicited; see {@link Ietf.solicitFromSetup}.
  #solicit;
  // The Hop IDs this session declared; see {@link Cluster}.
  #cluster;
  // Just to avoid logging when `close()` is called.
  #closed = false;
  /**
   * Creates a new Connection instance.
   * @param url - The URL of the connection
   * @param quic - The WebTransport session
   * @param control - The control/setup stream
   * @param maxRequestId - The initial max request ID
   * @param version - The negotiated protocol version
   * @param solicit - What the peer's SETUP declared (undefined when it declared nothing)
   * @param cluster - The Hop IDs the SETUP exchange settled, on the versions that negotiate them
   *
   * @internal
   */
  constructor({
    url,
    quic,
    control,
    maxRequestId,
    version: version2,
    client,
    discovery = true,
    solicit,
    cluster
  }) {
    this.url = url;
    this.discovery = discovery;
    this.version = versionName(version2);
    this.transport = transportOf(quic);
    this.#quic = quic;
    if (version2 >= Version.DRAFT_17) {
      this.#session = new NativeSession(quic, version2, client);
      void this.#runGoAway(control, version2);
    } else {
      const adapter = new ControlStreamAdapter(quic, control, version2, maxRequestId, client);
      this.#session = adapter;
      void adapter.run().catch((err) => {
        if (!this.#closed) console.error("adapter error", err);
        this.close();
      });
    }
    this.#publisher = new Publisher({
      quic: this.#quic,
      session: this.#session,
      requiresSolicitation: solicit ?? false,
      cluster
    });
    this.#solicit = solicit;
    this.#cluster = cluster;
    this.#subscriber = new Subscriber2({ session: this.#session, cluster });
    void this.#run();
  }
  /** Snapshot the transport's counters; see {@link Established.stats}. */
  async stats() {
    return transportStats(this.#quic);
  }
  /**
   * Closes the connection.
   */
  close() {
    if (this.#closed) return;
    this.#closed = true;
    this.#publisher.close();
    this.#session.close();
    try {
      this.#quic.close();
    } catch {
    }
  }
  async #run() {
    try {
      await Promise.all([
        this.#runBidis(),
        this.#runUnis(),
        this.#publisher.runPublishNamespaces(),
        this.#runDatagrams()
      ]);
    } catch (err) {
      if (!this.#closed) {
        console.error("fatal error running connection", err);
      }
    } finally {
      this.close();
    }
  }
  /**
   * Publishes a broadcast to the connection.
   * @param name - The broadcast path to publish
   * @param broadcast - The broadcast to publish
   */
  publish(path, producer) {
    this.#publisher.publish(path, producer);
  }
  /**
   * Gets an announced reader for the specified prefix.
   * @param prefix - The prefix for announcements
   * @returns An Announced instance
   */
  announced(prefix = empty()) {
    return this.#subscriber.announced(prefix);
  }
  /**
   * Consumes a broadcast from the connection.
   *
   * @remarks
   * If the broadcast is not found, a "not found" error will be thrown when requesting any tracks.
   *
   * @param broadcast - The path of the broadcast to consume
   * @returns A Broadcast instance
   */
  consume(path) {
    return this.#subscriber.consume(path);
  }
  /**
   * Watches a broadcast, live only while it is announced.
   *
   * @param path - The path of the broadcast to watch
   * @returns A reactive handle to the broadcast
   */
  announcedBroadcast(path) {
    return new Broadcast({ connection: this, path });
  }
  /**
   * Accepts bidi streams (virtual for v14-v16, real for v17) and dispatches.
   */
  async #runBidis() {
    for (; ; ) {
      const stream = await this.#session.acceptBi();
      if (!stream) break;
      void this.#runBidi(stream).catch((err) => {
        console.error("error processing bidi stream", err);
        stream.abort(new Error("bidi stream error"));
        if (err instanceof ProtocolViolation) this.close();
      });
    }
  }
  /**
   * Unified bidi stream dispatch. Reads typeId and routes to handler.
   * Matches the lite module's runBidi pattern.
   */
  async #runBidi(stream) {
    const typeId = await stream.reader.u53();
    switch (typeId) {
      // Draft-18 SUBSCRIBE_NAMESPACE (0x50) and the legacy 0x11 message decode
      // to the same request_id + namespace; the legacy options field is ignored.
      case SubscribeNamespace.id: {
        const msg = await SubscribeNamespace.decode(stream.reader, this.#session.version);
        await this.#publisher.runSubscribeNamespace(msg, stream);
        break;
      }
      case SubscribeNamespaceLegacy.id: {
        const legacy = await SubscribeNamespaceLegacy.decode(stream.reader, this.#session.version);
        const msg = new SubscribeNamespace({ requestId: legacy.requestId, namespace: legacy.namespace });
        await this.#publisher.runSubscribeNamespace(msg, stream);
        break;
      }
      case SubscribeUpdate.id: {
        stream.abort(new Error("unexpected REQUEST_UPDATE as initial message"));
        break;
      }
      // Publisher handles incoming requests
      case Subscribe.id: {
        const msg = await Subscribe.decode(stream.reader, this.#session.version);
        await this.#publisher.runSubscribe(msg, stream);
        break;
      }
      case TrackStatusRequest.id: {
        const msg = await TrackStatusRequest.decode(stream.reader, this.#session.version);
        await this.#publisher.runTrackStatusRequest(msg, stream);
        break;
      }
      // Subscriber handles incoming notifications
      case PublishNamespace.id: {
        const msg = await PublishNamespace.decode(
          stream.reader,
          this.#session.version,
          negotiated(this.#cluster)
        );
        const legacy = this.#session.version === Version.DRAFT_14 || this.#session.version === Version.DRAFT_15;
        if (this.#solicit !== void 0 && !legacy) {
          console.error(
            `unsolicited publish_namespace from a peer that implements MoQ Solicit: broadcast=${msg.trackNamespace}`
          );
          this.close();
          break;
        }
        await this.#subscriber.runPublishNamespace(msg, stream);
        break;
      }
      case Publish.id: {
        const msg = await Publish.decode(stream.reader, this.#session.version);
        await this.#subscriber.runPublish(msg, stream);
        break;
      }
      default:
        console.warn(`unexpected bidi stream type: 0x${typeId.toString(16)}`);
        stream.abort(new Error("unexpected stream type"));
    }
  }
  /**
   * Local patch: receive Draft 21 OBJECT_DATAGRAMs. Best-effort; never fails the session.
   */
  async #runDatagrams() {
    const reader = datagramReader(this.#quic);
    if (!reader) return;
    try {
      for (; ; ) {
        const { value, done } = await reader.read();
        if (done) return;
        this.#subscriber.handleDatagram(value);
      }
    } catch {
    } finally {
      try {
        reader.releaseLock();
      } catch {
      }
    }
  }
  /**
   * Handles unidirectional streams for media delivery (groups).
   */
  async #runUnis() {
    const readers = new Readers(this.#quic, this.#session.version);
    for (; ; ) {
      const stream = await readers.next();
      if (!stream) break;
      this.#runUni(stream).then(() => {
        stream.stop(new Error("cancel"));
      }).catch((err) => {
        console.error("error processing object stream", err);
        stream.stop(err);
      });
    }
  }
  async #runUni(stream) {
    const header = await Group.decode(stream, this.#session.version);
    await this.#subscriber.handleGroup(header, stream);
  }
  /**
   * v17+ only: reads GoAway from the setup/control stream.
   */
  async #runGoAway(controlStream, version2) {
    try {
      const done = await controlStream.reader.done();
      if (done) return;
      const typeId = await controlStream.reader.u53();
      if (typeId === GoAway.id) {
        const msg = await GoAway.decode(controlStream.reader, version2);
        console.warn(`received GOAWAY with redirect URI: ${msg.newSessionUri}`);
      } else {
        console.warn(`unexpected message on setup stream: 0x${typeId.toString(16)}`);
      }
    } catch (err) {
      if (!this.#closed) {
        console.error("error reading setup stream", err);
      }
    } finally {
      this.close();
    }
  }
  /**
   * Returns a promise that resolves when the connection is closed.
   * @returns A promise that resolves when closed
   */
  get closed() {
    return this.#quic.closed.then(() => void 0);
  }
};

// vendor/moq/js/net/src/ietf/setup.ts
var Setup = class _Setup {
  static id = 12032;
  parameters;
  constructor({ parameters = new SetupOptions() } = {}) {
    this.parameters = parameters;
  }
  async #encode(w, version2) {
    await this.parameters.encode(w, version2);
  }
  async encode(w, version2) {
    return encode6(w, (mw) => this.#encode(mw, version2));
  }
  static async #decode(r, version2) {
    const parameters = await SetupOptions.decode(r, version2);
    return new _Setup({ parameters });
  }
  static async decode(r, version2) {
    return decode6(r, (mr) => _Setup.#decode(mr, version2));
  }
};
var MAX_VERSIONS = 128;
var ClientSetup = class _ClientSetup {
  static id = 32;
  versions;
  parameters;
  constructor({ versions, parameters = new SetupOptions() }) {
    this.versions = versions;
    this.parameters = parameters;
  }
  async #encode(w, version2) {
    if (version2 === Version.DRAFT_15 || version2 === Version.DRAFT_16) {
      await this.parameters.encode(w, version2);
    } else if (version2 === Version.DRAFT_14) {
      await w.u53(this.versions.length);
      for (const v of this.versions) {
        await w.u53(v);
      }
      await this.parameters.encode(w, version2);
    } else {
      throw new Error("ClientSetup not used for this version");
    }
  }
  async encode(w, version2) {
    return encode6(w, (mw) => this.#encode(mw, version2));
  }
  static async #decode(r, version2) {
    if (version2 === Version.DRAFT_15 || version2 === Version.DRAFT_16) {
      const parameters = await SetupOptions.decode(r, version2);
      return new _ClientSetup({ versions: [version2], parameters });
    } else if (version2 === Version.DRAFT_14) {
      const numVersions = await r.u53();
      if (numVersions > MAX_VERSIONS) {
        throw new Error(`too many versions: ${numVersions}`);
      }
      const supportedVersions = [];
      for (let i = 0; i < numVersions; i++) {
        const v = await r.u53();
        supportedVersions.push(v);
      }
      const parameters = await SetupOptions.decode(r, version2);
      return new _ClientSetup({ versions: supportedVersions, parameters });
    } else {
      throw new Error("ClientSetup not used for this version");
    }
  }
  static async decode(r, version2) {
    return decode6(r, (mr) => _ClientSetup.#decode(mr, version2));
  }
};
var ServerSetup = class _ServerSetup {
  static id = 33;
  version;
  parameters;
  constructor({ version: version2, parameters = new SetupOptions() }) {
    this.version = version2;
    this.parameters = parameters;
  }
  async #encode(w, version2) {
    if (version2 === Version.DRAFT_15 || version2 === Version.DRAFT_16) {
      await this.parameters.encode(w, version2);
    } else if (version2 === Version.DRAFT_14) {
      await w.u53(this.version);
      await this.parameters.encode(w, version2);
    } else {
      throw new Error("ServerSetup not used for this version");
    }
  }
  async encode(w, version2) {
    return encode6(w, (mw) => this.#encode(mw, version2));
  }
  static async #decode(r, version2) {
    if (version2 === Version.DRAFT_15 || version2 === Version.DRAFT_16) {
      const parameters = await SetupOptions.decode(r, version2);
      return new _ServerSetup({ version: version2, parameters });
    } else if (version2 === Version.DRAFT_14) {
      const selectedVersion = await r.u53();
      const parameters = await SetupOptions.decode(r, version2);
      return new _ServerSetup({ version: selectedVersion, parameters });
    } else {
      throw new Error("ServerSetup not used for this version");
    }
  }
  static async decode(r, version2) {
    return decode6(r, (mr) => _ServerSetup.#decode(mr, version2));
  }
};

// vendor/moq/js/net/src/ietf/solicit.ts
function solicitFromSetup(params) {
  const value = params.getVarint(SetupOption.Solicit);
  return value === void 0 ? void 0 : value !== 0n;
}
function solicitIntoSetup(params) {
  params.setVarint(SetupOption.Solicit, 1n);
}

// vendor/moq/js/net/src/lite/message.ts
async function encode9(writer, f) {
  let scratch = new Uint8Array();
  const temp = new Writer(
    new WritableStream({
      write(chunk) {
        const needed = scratch.byteLength + chunk.byteLength;
        if (needed > scratch.buffer.byteLength) {
          const capacity = Math.max(needed, scratch.buffer.byteLength * 2);
          const newBuffer = new ArrayBuffer(capacity);
          const newScratch = new Uint8Array(newBuffer, 0, needed);
          newScratch.set(scratch);
          newScratch.set(chunk, scratch.byteLength);
          scratch = newScratch;
        } else {
          scratch = new Uint8Array(scratch.buffer, 0, needed);
          scratch.set(chunk, needed - chunk.byteLength);
        }
      }
    })
  );
  await f(temp);
  temp.close();
  await temp.closed;
  await writer.u53(scratch.byteLength);
  if (scratch.byteLength > 0) {
    await writer.write(scratch);
  }
}
async function decode9(reader, f) {
  const size2 = await reader.u53();
  const data = await reader.read(size2);
  const limit = new Reader(void 0, data);
  const msg = await f(limit);
  if (!await limit.done()) {
    throw new Error("Message decoding consumed too few bytes");
  }
  return msg;
}
async function decodeMaybe(reader, f) {
  if (await reader.done()) return;
  return await decode9(reader, f);
}

// vendor/moq/js/net/src/lite/version.ts
var Version2 = {
  DRAFT_01: 4279086337,
  DRAFT_02: 4279086338,
  DRAFT_03: 4279086339,
  DRAFT_04: 4279086340,
  DRAFT_05: 4279086341,
  /// Work-in-progress lite-06, advertised as the preferred WebTransport subprotocol.
  /// Adds announce ids: each active ANNOUNCE_BROADCAST implicitly assigns the next
  /// ordinal, and ended/restart reference that id instead of repeating the path.
  DRAFT_06: 4279086342
};
function hasProbeRtt(version2) {
  switch (version2) {
    case Version2.DRAFT_01:
    case Version2.DRAFT_02:
    case Version2.DRAFT_03:
      return false;
    default:
      return true;
  }
}
function hasSetupStream(version2) {
  switch (version2) {
    case Version2.DRAFT_01:
    case Version2.DRAFT_02:
    case Version2.DRAFT_03:
    case Version2.DRAFT_04:
      return false;
    default:
      return true;
  }
}
function hasDatagrams(version2) {
  switch (version2) {
    case Version2.DRAFT_01:
    case Version2.DRAFT_02:
    case Version2.DRAFT_03:
    case Version2.DRAFT_04:
      return false;
    default:
      return true;
  }
}
function hasAnnounceOk(version2) {
  switch (version2) {
    case Version2.DRAFT_01:
    case Version2.DRAFT_02:
    case Version2.DRAFT_03:
    case Version2.DRAFT_04:
      return false;
    default:
      return true;
  }
}
function restartSupported(version2) {
  switch (version2) {
    case Version2.DRAFT_01:
    case Version2.DRAFT_02:
    case Version2.DRAFT_03:
    case Version2.DRAFT_04:
      return false;
    default:
      return true;
  }
}
function hasAnnounceId(version2) {
  switch (version2) {
    case Version2.DRAFT_01:
    case Version2.DRAFT_02:
    case Version2.DRAFT_03:
    case Version2.DRAFT_04:
    case Version2.DRAFT_05:
      return false;
    default:
      return true;
  }
}
function hasExcludeHop(version2) {
  switch (version2) {
    case Version2.DRAFT_04:
    case Version2.DRAFT_05:
      return true;
    default:
      return false;
  }
}
function hasRouteCost(version2) {
  switch (version2) {
    case Version2.DRAFT_01:
    case Version2.DRAFT_02:
    case Version2.DRAFT_03:
    case Version2.DRAFT_04:
    case Version2.DRAFT_05:
      return false;
    default:
      return true;
  }
}
var ALPN2 = "moql";
var ALPN_03 = "moq-lite-03";
var ALPN_04 = "moq-lite-04";
var ALPN_05 = "moq-lite-05";
var ALPN_06_WIP = "moq-lite-06-wip";
var VERSION_NAMES2 = {
  [Version2.DRAFT_01]: "moq-lite-01",
  [Version2.DRAFT_02]: "moq-lite-02",
  [Version2.DRAFT_03]: "moq-lite-03",
  [Version2.DRAFT_04]: "moq-lite-04",
  [Version2.DRAFT_05]: "moq-lite-05",
  [Version2.DRAFT_06]: "moq-lite-06-wip"
};
function versionName2(v) {
  return VERSION_NAMES2[v] ?? `unknown(0x${v.toString(16)})`;
}

// vendor/moq/js/net/src/lite/announce.ts
var STATUS_ENDED = 0;
var STATUS_ACTIVE = 1;
var STATUS_RESTART = 2;
var ANNOUNCE_START = 0;
var ANNOUNCE_END = 1;
var ANNOUNCE_RESTART = 2;
function checkHops(hops) {
  if (hops.length > MAX_HOPS) {
    throw new Error(`hop count ${hops.length} exceeds maximum ${MAX_HOPS}`);
  }
}
async function encodeHops(w, version2, hops) {
  checkHops(hops);
  switch (version2) {
    case Version2.DRAFT_01:
    case Version2.DRAFT_02:
      break;
    case Version2.DRAFT_03:
      await w.u53(hops.length);
      break;
    default:
      await w.u53(hops.length);
      for (const origin of hops) {
        await w.u62(origin);
      }
      break;
  }
}
async function decodeHops(r, version2) {
  switch (version2) {
    case Version2.DRAFT_01:
    case Version2.DRAFT_02:
      return [];
    case Version2.DRAFT_03: {
      const count = await r.u53();
      if (count > MAX_HOPS) throw new Error(`hop count ${count} exceeds maximum ${MAX_HOPS}`);
      return new Array(count).fill(UNKNOWN_ORIGIN);
    }
    default: {
      const count = await r.u53();
      if (count > MAX_HOPS) throw new Error(`hop count ${count} exceeds maximum ${MAX_HOPS}`);
      const hops = [];
      for (let i = 0; i < count; i++) {
        hops.push(OriginSchema.parse(await r.u62()));
      }
      return hops;
    }
  }
}
async function encodeRouteCost(w, version2, cost) {
  if (!hasRouteCost(version2)) return;
  await w.u62(cost ?? 0n);
}
async function decodeRouteCost(r, version2) {
  if (!hasRouteCost(version2)) return 0n;
  return await r.u62();
}
async function encodeAnnounce06Body(w, msg, version2) {
  switch (msg.status) {
    case "active":
      await w.string(encode(msg.suffix));
      await encodeHops(w, version2, msg.hops);
      await encodeRouteCost(w, version2, msg.cost);
      break;
    case "endedId":
      await w.u62(msg.id);
      break;
    case "restart":
      await w.u62(msg.id);
      await encodeHops(w, version2, msg.hops);
      await encodeRouteCost(w, version2, msg.cost);
      break;
    case "ended":
      throw new Error("ended-by-path not supported for this version");
  }
}
function announce06Type(msg) {
  switch (msg.status) {
    case "active":
      return ANNOUNCE_START;
    case "endedId":
      return ANNOUNCE_END;
    case "restart":
      return ANNOUNCE_RESTART;
    case "ended":
      throw new Error("ended-by-path not supported for this version");
  }
}
async function decodeAnnounce06Body(r, typ, version2) {
  switch (typ) {
    case ANNOUNCE_START: {
      const suffix = decode(await r.string());
      const hops = await decodeHops(r, version2);
      return { status: "active", suffix, hops, cost: await decodeRouteCost(r, version2) };
    }
    case ANNOUNCE_END:
      return { status: "endedId", id: await r.u62() };
    case ANNOUNCE_RESTART: {
      const id = await r.u62();
      const hops = await decodeHops(r, version2);
      return { status: "restart", id, hops, cost: await decodeRouteCost(r, version2) };
    }
    default:
      throw new Error(`unknown announce message type: ${typ}`);
  }
}
async function encodeLegacyBody(w, msg, version2) {
  switch (msg.status) {
    case "active":
      await w.u8(STATUS_ACTIVE);
      await w.string(encode(msg.suffix));
      await encodeHops(w, version2, msg.hops);
      break;
    case "ended":
      await w.u8(STATUS_ENDED);
      await w.string(encode(msg.suffix));
      await encodeHops(w, version2, []);
      break;
    case "endedId":
    case "restart":
      throw new Error("announce ids not supported for this version");
  }
}
async function decodeLegacyBody(r, version2) {
  const status = await r.u8();
  const active = status === STATUS_ACTIVE || status === STATUS_RESTART && hasAnnounceOk(version2);
  if (status !== STATUS_ENDED && !active) {
    throw new Error("invalid announce status");
  }
  const suffix = decode(await r.string());
  const hops = await decodeHops(r, version2);
  return active ? { status: "active", suffix, hops } : { status: "ended", suffix };
}
async function encodeAnnounceBroadcast(w, msg, version2) {
  if (hasAnnounceId(version2)) {
    await w.u53(announce06Type(msg));
    return encode9(w, (w2) => encodeAnnounce06Body(w2, msg, version2));
  }
  return encode9(w, (w2) => encodeLegacyBody(w2, msg, version2));
}
async function decodeAnnounceBroadcastMaybe(r, version2) {
  if (hasAnnounceId(version2)) {
    if (await r.done()) return void 0;
    const typ = await r.u53();
    return decode9(r, (r2) => decodeAnnounce06Body(r2, typ, version2));
  }
  return decodeMaybe(r, (r2) => decodeLegacyBody(r2, version2));
}
var AnnounceRequest = class _AnnounceRequest {
  prefix;
  /** Lite04/05 only: the 62-bit Origin id of the peer asking for announces, which the
   * publisher uses to skip announces that already passed through it. Zero means "no
   * exclusion". Not on the wire elsewhere, so a value set here is ignored when encoding
   * for another version and decodes as zero.
   *
   * Must be a bigint: peer origins are up to 62 bits and overflow u53. */
  excludeHop;
  constructor(prefix, excludeHop = 0n) {
    this.prefix = prefix;
    this.excludeHop = excludeHop;
  }
  async #encode(w, version2) {
    await w.string(encode(this.prefix));
    if (hasExcludeHop(version2)) {
      await w.u62(this.excludeHop);
    }
  }
  static async #decode(r, version2) {
    const prefix = decode(await r.string());
    const excludeHop = hasExcludeHop(version2) ? await r.u62() : 0n;
    return new _AnnounceRequest(prefix, excludeHop);
  }
  async encode(w, version2) {
    return encode9(w, (w2) => this.#encode(w2, version2));
  }
  static async decode(r, version2) {
    return decode9(r, (r2) => _AnnounceRequest.#decode(r2, version2));
  }
};
var AnnounceInit = class _AnnounceInit {
  suffixes;
  constructor(paths) {
    this.suffixes = paths;
  }
  static #guard(version2) {
    switch (version2) {
      case Version2.DRAFT_01:
      case Version2.DRAFT_02:
        break;
      default:
        throw new Error("announce init not supported for this version");
    }
  }
  async #encode(w) {
    await w.u53(this.suffixes.length);
    for (const path of this.suffixes) {
      await w.string(encode(path));
    }
  }
  static async #decode(r) {
    const count = await r.u53();
    const suffixes = [];
    for (let i = 0; i < count; i++) {
      suffixes.push(decode(await r.string()));
    }
    return new _AnnounceInit(suffixes);
  }
  async encode(w, version2) {
    _AnnounceInit.#guard(version2);
    return encode9(w, this.#encode.bind(this));
  }
  static async decode(r, version2) {
    _AnnounceInit.#guard(version2);
    return decode9(r, _AnnounceInit.#decode);
  }
};
var AnnounceOk = class _AnnounceOk {
  origin;
  active;
  constructor(origin, active) {
    this.origin = origin;
    this.active = active;
  }
  static #guard(version2) {
    if (!hasAnnounceOk(version2)) {
      throw new Error("announce ok not supported for this version");
    }
  }
  async #encode(w) {
    await w.u62(this.origin);
    await w.u53(this.active);
  }
  static async #decode(r) {
    const origin = OriginSchema.parse(await r.u62());
    const active = await r.u53();
    return new _AnnounceOk(origin, active);
  }
  async encode(w, version2) {
    _AnnounceOk.#guard(version2);
    return encode9(w, this.#encode.bind(this));
  }
  static async decode(r, version2) {
    _AnnounceOk.#guard(version2);
    return decode9(r, _AnnounceOk.#decode);
  }
};

// vendor/moq/js/net/src/lite/fetch.ts
function guardFetch(version2) {
  switch (version2) {
    case Version2.DRAFT_01:
    case Version2.DRAFT_02:
      throw new Error("fetch not supported for this version");
    default:
      break;
  }
}
var Fetch = class _Fetch {
  broadcast;
  track;
  priority;
  group;
  constructor(broadcast, track, priority, group) {
    this.broadcast = broadcast;
    this.track = track;
    this.priority = priority;
    this.group = group;
  }
  async #encode(w) {
    await w.string(encode(this.broadcast));
    await w.string(this.track);
    await w.u8(this.priority);
    await w.u53(this.group);
  }
  static async #decode(r) {
    const broadcast = decode(await r.string());
    const track = await r.string();
    const priority = await r.u8();
    const group = await r.u53();
    return new _Fetch(broadcast, track, priority, group);
  }
  async encode(w, version2) {
    guardFetch(version2);
    return encode9(w, (w2) => this.#encode(w2));
  }
  static async decode(r, version2) {
    guardFetch(version2);
    return decode9(r, (r2) => _Fetch.#decode(r2));
  }
};

// vendor/moq/js/net/src/lite/goaway.ts
function guardGoaway(version2) {
  switch (version2) {
    case Version2.DRAFT_01:
    case Version2.DRAFT_02:
    case Version2.DRAFT_03:
      throw new Error("goaway not supported for this version");
    default:
      break;
  }
}
var Goaway = class _Goaway {
  uri;
  constructor(uri) {
    this.uri = uri;
  }
  async #encode(w) {
    await w.string(this.uri);
  }
  static async #decode(r) {
    return new _Goaway(await r.string());
  }
  async encode(w, version2) {
    guardGoaway(version2);
    return encode9(w, this.#encode.bind(this));
  }
  static async decode(r, version2) {
    guardGoaway(version2);
    return decode9(r, _Goaway.#decode);
  }
};

// vendor/moq/js/net/src/lite/group.ts
var Group2 = class _Group {
  subscribe;
  sequence;
  constructor(subscribe2, sequence) {
    this.subscribe = subscribe2;
    this.sequence = sequence;
  }
  async #encode(w) {
    await w.u62(this.subscribe);
    await w.u53(this.sequence);
  }
  static async #decode(r) {
    return new _Group(await r.u62(), await r.u53());
  }
  async encode(w) {
    return encode9(w, this.#encode.bind(this));
  }
  static async decode(r) {
    return decode9(r, _Group.#decode);
  }
  static async decodeMaybe(r) {
    return decodeMaybe(r, _Group.#decode);
  }
};

// vendor/moq/js/net/src/lite/datagram.ts
var Datagram = class _Datagram {
  /** Subscribe ID this datagram is delivered on. */
  subscribe;
  /** Group sequence number (shared with the track's group namespace). */
  sequence;
  /** Absolute presentation timestamp, in the track's negotiated timescale. */
  timestamp;
  /** The frame payload, delimited by the datagram boundary. */
  payload;
  constructor(subscribe2, sequence, timestamp, payload) {
    this.subscribe = subscribe2;
    this.sequence = sequence;
    this.timestamp = timestamp;
    this.payload = payload;
  }
  /** Encode the body to a single `Uint8Array` (no length prefix; the datagram boundary delimits it). */
  encode() {
    const subscribe2 = encodeTo(new ArrayBuffer(8), this.subscribe);
    const sequence = encodeTo(new ArrayBuffer(8), this.sequence);
    const timestamp = encodeTo(new ArrayBuffer(8), this.timestamp);
    const out = new Uint8Array(
      subscribe2.byteLength + sequence.byteLength + timestamp.byteLength + this.payload.byteLength
    );
    let offset = 0;
    out.set(subscribe2, offset);
    offset += subscribe2.byteLength;
    out.set(sequence, offset);
    offset += sequence.byteLength;
    out.set(timestamp, offset);
    offset += timestamp.byteLength;
    out.set(this.payload, offset);
    return out;
  }
  /** Decode a datagram body from the raw bytes of one QUIC datagram. */
  static async decode(data) {
    const r = new Reader(void 0, data);
    const subscribe2 = await r.u62();
    const sequence = await r.u53();
    const timestamp = await r.u53();
    const payload = await r.readAll();
    return new _Datagram(subscribe2, sequence, timestamp, payload);
  }
};

// vendor/moq/js/net/src/lite/priority.ts
var GROUP_SPAN = 2 ** 44;
var MAX_PRIORITY = 255;
function sendOrder({ priority, position = 0 }) {
  return -((MAX_PRIORITY - clamp(priority, MAX_PRIORITY)) * GROUP_SPAN + clamp(position, GROUP_SPAN - 1) + 1);
}
function clamp(value, max) {
  return Math.min(Math.max(Math.trunc(value), 0), max);
}
var Priority = class {
  #track;
  #streams = /* @__PURE__ */ new Map();
  #dispose;
  #closed = false;
  /** Follow `track`'s subscription until {@link close}. */
  constructor(track) {
    this.#track = track;
    this.#dispose = track.subscription.subscribe(() => this.#rerank());
  }
  /** The send order for a group at `sequence`, whether or not it has a stream yet. */
  rank(sequence) {
    return sendOrder({
      priority: this.#track.subscription.peek()?.priority ?? 0,
      position: this.#position(sequence)
    });
  }
  /** Rank a group's stream now, and again whenever the ranking changes, until {@link remove}. */
  add(stream, sequence) {
    this.#streams.set(stream, sequence);
    this.#rerank();
  }
  /** Stop ranking a finished group's stream, promoting whatever was queued behind it. */
  remove(stream) {
    if (!this.#streams.delete(stream)) return;
    if (this.#closed && this.#streams.size === 0) {
      this.#dispose();
    } else {
      this.#rerank();
    }
  }
  /**
   * Stop taking new groups, and release the subscription listener once the last one leaves.
   *
   * The track can finish while several groups are still draining, and those are the ones the
   * subscriber is still waiting on. They keep being promoted as the groups ahead of them
   * finish, rather than being stranded at whatever position they held when the track ended.
   */
  close() {
    this.#closed = true;
    if (this.#streams.size === 0) this.#dispose();
  }
  // How many of this subscription's groups in flight should be sent before `sequence`.
  // Sequences are unique within a subscription, so a group already registered never counts
  // itself.
  #position(sequence) {
    const ordered = this.#track.subscription.peek()?.ordered ?? false;
    let ahead = 0;
    for (const other of this.#streams.values()) {
      if (ordered ? other < sequence : other > sequence) ahead++;
    }
    return ahead;
  }
  #rerank() {
    const priority = this.#track.subscription.peek()?.priority ?? 0;
    for (const [stream, sequence] of this.#streams) {
      stream.setPriority(sendOrder({ priority, position: this.#position(sequence) }));
    }
  }
};

// vendor/moq/js/net/src/lite/probe.ts
function guardProbe(version2) {
  switch (version2) {
    case Version2.DRAFT_01:
    case Version2.DRAFT_02:
      throw new Error("probe not supported for this version");
    default:
      break;
  }
}
var Probe = class _Probe {
  /** Estimated send bitrate in bits per second, or undefined if unknown. */
  bitrate;
  /** Smoothed round-trip time in milliseconds, or undefined if unknown. */
  rtt;
  // Named rather than positional: the two fields share a type, so positional
  // arguments could be swapped without a type error.
  constructor({ bitrate, rtt } = {}) {
    this.bitrate = bitrate;
    this.rtt = rtt;
  }
  async #encode(w, version2) {
    await w.u53(this.bitrate !== void 0 ? Math.max(this.bitrate, 1) : 0);
    if (hasProbeRtt(version2)) {
      await w.u53(this.rtt !== void 0 ? Math.max(this.rtt, 1) : 0);
    }
  }
  static async #decode(r, version2) {
    const bitrateWire = await r.u53();
    const bitrate = bitrateWire === 0 ? void 0 : bitrateWire;
    let rtt;
    if (hasProbeRtt(version2)) {
      const wire = await r.u53();
      rtt = wire === 0 ? void 0 : wire;
    }
    return new _Probe({ bitrate, rtt });
  }
  async encode(w, version2) {
    guardProbe(version2);
    return encode9(w, (w2) => this.#encode(w2, version2));
  }
  static async decode(r, version2) {
    guardProbe(version2);
    return decode9(r, (r2) => _Probe.#decode(r2, version2));
  }
  static async decodeMaybe(r, version2) {
    guardProbe(version2);
    return decodeMaybe(r, (r2) => _Probe.#decode(r2, version2));
  }
};

// vendor/moq/js/net/src/lite/subscribe.ts
var SubscribeUpdate2 = class _SubscribeUpdate {
  priority;
  ordered;
  maxLatency;
  startGroup;
  endGroup;
  constructor(props) {
    this.priority = props.priority;
    this.ordered = props.ordered ?? false;
    this.maxLatency = props.maxLatency ?? 0;
    this.startGroup = props.startGroup;
    this.endGroup = props.endGroup;
  }
  async #encode(w, version2) {
    switch (version2) {
      case Version2.DRAFT_01:
      case Version2.DRAFT_02:
        await w.u8(this.priority);
        break;
      default:
        await w.u8(this.priority);
        await w.bool(this.ordered);
        await w.u53(this.maxLatency);
        await w.u53(this.startGroup !== void 0 ? this.startGroup + 1 : 0);
        await w.u53(this.endGroup !== void 0 ? this.endGroup + 1 : 0);
        break;
    }
  }
  static async #decode(r, version2) {
    switch (version2) {
      case Version2.DRAFT_01:
      case Version2.DRAFT_02:
        return new _SubscribeUpdate({ priority: await r.u8() });
      default: {
        const priority = await r.u8();
        const ordered = await r.bool();
        const maxLatency = await r.u53();
        const startGroup = await r.u53();
        const endGroup = await r.u53();
        return new _SubscribeUpdate({
          priority,
          ordered,
          maxLatency,
          startGroup: startGroup > 0 ? startGroup - 1 : void 0,
          endGroup: endGroup > 0 ? endGroup - 1 : void 0
        });
      }
    }
  }
  async encode(w, version2) {
    return encode9(w, (w2) => this.#encode(w2, version2));
  }
  static async decode(r, version2) {
    return decode9(r, (r2) => _SubscribeUpdate.#decode(r2, version2));
  }
  static async decodeMaybe(r, version2) {
    return decodeMaybe(r, (r2) => _SubscribeUpdate.#decode(r2, version2));
  }
};
var Subscribe2 = class _Subscribe {
  id;
  broadcast;
  track;
  priority;
  ordered;
  maxLatency;
  startGroup;
  endGroup;
  constructor(props) {
    this.id = props.id;
    this.broadcast = props.broadcast;
    this.track = props.track;
    this.priority = props.priority;
    this.ordered = props.ordered ?? false;
    this.maxLatency = props.maxLatency ?? 0;
    this.startGroup = props.startGroup;
    this.endGroup = props.endGroup;
  }
  async #encode(w, version2) {
    await w.u62(this.id);
    await w.string(encode(this.broadcast));
    await w.string(this.track);
    await w.u8(this.priority);
    switch (version2) {
      case Version2.DRAFT_01:
      case Version2.DRAFT_02:
        break;
      default:
        await w.bool(this.ordered);
        await w.u53(this.maxLatency);
        await w.u53(this.startGroup !== void 0 ? this.startGroup + 1 : 0);
        await w.u53(this.endGroup !== void 0 ? this.endGroup + 1 : 0);
        break;
    }
  }
  static async #decode(r, version2) {
    const id = await r.u62();
    const broadcast = decode(await r.string());
    const track = await r.string();
    const priority = await r.u8();
    switch (version2) {
      case Version2.DRAFT_01:
      case Version2.DRAFT_02:
        return new _Subscribe({ id, broadcast, track, priority });
      default: {
        const ordered = await r.bool();
        const maxLatency = await r.u53();
        const startGroup = await r.u53();
        const endGroup = await r.u53();
        return new _Subscribe({
          id,
          broadcast,
          track,
          priority,
          ordered,
          maxLatency,
          startGroup: startGroup > 0 ? startGroup - 1 : void 0,
          endGroup: endGroup > 0 ? endGroup - 1 : void 0
        });
      }
    }
  }
  async encode(w, version2) {
    return encode9(w, (w2) => this.#encode(w2, version2));
  }
  static async decode(r, version2) {
    return decode9(r, (r2) => _Subscribe.#decode(r2, version2));
  }
};
var SubscribeOk2 = class _SubscribeOk {
  priority;
  ordered;
  maxLatency;
  startGroup;
  endGroup;
  constructor({
    priority = 0,
    ordered = false,
    maxLatency = 0,
    startGroup = void 0,
    endGroup = void 0
  }) {
    this.priority = priority;
    this.ordered = ordered;
    this.maxLatency = maxLatency;
    this.startGroup = startGroup;
    this.endGroup = endGroup;
  }
  async #encode(w, version2) {
    switch (version2) {
      case Version2.DRAFT_02:
        break;
      case Version2.DRAFT_01:
        await w.u8(this.priority ?? 0);
        break;
      // Draft-05+ never sends SUBSCRIBE_OK, but keep the field layout matching
      // Draft-03/04 so a stray future use stays well-formed.
      default:
        await w.u8(this.priority);
        await w.bool(this.ordered);
        await w.u53(this.maxLatency);
        await w.u53(this.startGroup !== void 0 ? this.startGroup + 1 : 0);
        await w.u53(this.endGroup !== void 0 ? this.endGroup + 1 : 0);
        break;
    }
  }
  static async #decode(version2, r) {
    let priority;
    let ordered;
    let maxLatency;
    let startGroup;
    let endGroup;
    switch (version2) {
      case Version2.DRAFT_02:
        break;
      case Version2.DRAFT_01:
        priority = await r.u8();
        break;
      default:
        priority = await r.u8();
        ordered = await r.bool();
        maxLatency = await r.u53();
        startGroup = await r.u53();
        endGroup = await r.u53();
        break;
    }
    return new _SubscribeOk({
      priority,
      ordered,
      maxLatency,
      startGroup: startGroup !== void 0 && startGroup > 0 ? startGroup - 1 : void 0,
      endGroup: endGroup !== void 0 && endGroup > 0 ? endGroup - 1 : void 0
    });
  }
  async encode(w, version2) {
    return encode9(w, (w2) => this.#encode(w2, version2));
  }
  static async decode(r, version2) {
    return decode9(r, _SubscribeOk.#decode.bind(_SubscribeOk, version2));
  }
};
var SubscribeStart = class _SubscribeStart {
  group;
  constructor(group) {
    this.group = group;
  }
  async encode(w) {
    return encode9(w, async (w2) => {
      await w2.u53(this.group);
    });
  }
  static async decode(r) {
    return decode9(r, async (r2) => new _SubscribeStart(await r2.u53()));
  }
};
var SubscribeEnd = class _SubscribeEnd {
  /** The exclusive final group sequence: the first sequence that will never be produced. */
  group;
  constructor(group) {
    this.group = group;
  }
  async encode(w) {
    return encode9(w, async (w2) => {
      await w2.u53(this.group);
    });
  }
  static async decode(r) {
    return decode9(r, async (r2) => new _SubscribeEnd(await r2.u53()));
  }
};
var SubscribeDrop = class _SubscribeDrop {
  start;
  end;
  error;
  constructor(props) {
    this.start = props.start;
    this.end = props.end;
    this.error = props.error;
  }
  async #encode(w) {
    await w.u53(this.start);
    await w.u53(this.end);
    await w.u53(this.error);
  }
  static async #decode(r) {
    return new _SubscribeDrop({ start: await r.u53(), end: await r.u53(), error: await r.u53() });
  }
  async encode(w) {
    return encode9(w, this.#encode.bind(this));
  }
  static async decode(r) {
    return decode9(r, _SubscribeDrop.#decode);
  }
};
async function encodeSubscribeResponse(w, resp, version2) {
  switch (version2) {
    case Version2.DRAFT_01:
    case Version2.DRAFT_02:
      if ("ok" in resp) {
        await resp.ok.encode(w, version2);
      } else {
        throw new Error("only SUBSCRIBE_OK is supported for this version");
      }
      break;
    case Version2.DRAFT_03:
    case Version2.DRAFT_04:
      if ("ok" in resp) {
        await w.u53(0);
        await resp.ok.encode(w, version2);
      } else if ("drop" in resp) {
        await w.u53(1);
        await resp.drop.encode(w);
      } else {
        throw new Error("SUBSCRIBE_START/END not supported for this version");
      }
      break;
    default:
      if ("start" in resp) {
        await w.u53(0);
        await resp.start.encode(w);
      } else if ("end" in resp) {
        await w.u53(1);
        await resp.end.encode(w);
      } else if ("drop" in resp) {
        await w.u53(2);
        await resp.drop.encode(w);
      } else {
        throw new Error("SUBSCRIBE_OK not supported for this version");
      }
      break;
  }
}
async function decodeSubscribeResponse(r, version2) {
  switch (version2) {
    case Version2.DRAFT_01:
    case Version2.DRAFT_02:
      return { ok: await SubscribeOk2.decode(r, version2) };
    case Version2.DRAFT_03:
    case Version2.DRAFT_04: {
      const typ = await r.u53();
      switch (typ) {
        case 0:
          return { ok: await SubscribeOk2.decode(r, version2) };
        case 1:
          return { drop: await SubscribeDrop.decode(r) };
        default:
          throw new Error(`unknown subscribe response type: ${typ}`);
      }
    }
    default: {
      const typ = await r.u53();
      switch (typ) {
        case 0:
          return { start: await SubscribeStart.decode(r) };
        case 1:
          return { end: await SubscribeEnd.decode(r) };
        case 2:
          return { drop: await SubscribeDrop.decode(r) };
        default:
          throw new Error(`unknown subscribe response type: ${typ}`);
      }
    }
  }
}
async function decodeSubscribeResponseMaybe(r, version2) {
  if (await r.done()) return void 0;
  return decodeSubscribeResponse(r, version2);
}

// vendor/moq/js/net/src/lite/track.ts
function guardTrack(version2) {
  switch (version2) {
    case Version2.DRAFT_01:
    case Version2.DRAFT_02:
    case Version2.DRAFT_03:
    case Version2.DRAFT_04:
      throw new Error("track stream not supported for this version");
    default:
      break;
  }
}
var Track = class _Track {
  broadcast;
  track;
  constructor(broadcast, track) {
    this.broadcast = broadcast;
    this.track = track;
  }
  async #encode(w) {
    await w.string(encode(this.broadcast));
    await w.string(this.track);
  }
  static async #decode(r) {
    const broadcast = decode(await r.string());
    const track = await r.string();
    return new _Track(broadcast, track);
  }
  async encode(w, version2) {
    guardTrack(version2);
    return encode9(w, (w2) => this.#encode(w2));
  }
  static async decode(r, version2) {
    guardTrack(version2);
    return decode9(r, (r2) => _Track.#decode(r2));
  }
};
var TrackInfo = class _TrackInfo {
  /** The publisher's tie-break priority for this track. */
  priority;
  /**
   * Whether groups are prioritized in sequence order. Groups may always arrive
   * out-of-order (or not at all) over the network.
   */
  ordered;
  /**
   * Publisher Max Latency: an upper bound (milliseconds) on how long the publisher
   * caches a non-latest group past the arrival of a newer one.
   */
  latencyMax;
  /**
   * Per-frame timestamp scale (units per second). Mandatory on Lite05: a real
   * (non-zero) scale, and every frame on the wire is prefixed with a zigzag-delta
   * timestamp at this scale.
   */
  timescale;
  constructor({
    priority = 0,
    ordered = false,
    latencyMax = 0,
    timescale = 0
  }) {
    this.priority = priority;
    this.ordered = ordered;
    this.latencyMax = latencyMax;
    this.timescale = timescale;
  }
  async #encode(w) {
    await w.u8(this.priority);
    await w.bool(this.ordered);
    await w.u53(this.latencyMax);
    await w.u53(this.timescale);
  }
  static async #decode(r) {
    const priority = await r.u8();
    const ordered = await r.bool();
    const latencyMax = await r.u53();
    const timescale = await r.u53();
    if (timescale === 0) throw new Error("track timescale must be non-zero");
    return new _TrackInfo({ priority, ordered, latencyMax, timescale });
  }
  async encode(w, version2) {
    guardTrack(version2);
    if (this.timescale === 0) throw new Error("track timescale must be non-zero");
    return encode9(w, (w2) => this.#encode(w2));
  }
  static async decode(r, version2) {
    guardTrack(version2);
    return decode9(r, (r2) => _TrackInfo.#decode(r2));
  }
};

// vendor/moq/js/net/src/lite/publisher.ts
var PROBE_INTERVAL = 100;
var PROBE_MAX_AGE = 1e4;
var PROBE_MAX_DELTA = 0.25;
var PROBE_RTT_DELTA = 0.25;
function zigzag(delta) {
  return delta >= 0n ? delta << 1n : (-delta << 1n) - 1n;
}
function supportsTrackStream(version2) {
  switch (version2) {
    case Version2.DRAFT_01:
    case Version2.DRAFT_02:
    case Version2.DRAFT_03:
    case Version2.DRAFT_04:
      return false;
    default:
      return true;
  }
}
var Publisher2 = class {
  // The version of the connection.
  version;
  // Per-connection origin appended to outbound Announce hops, so the peer
  // can detect loops and prefer shorter paths. Created by Connection and
  // shared with Subscriber, which can optionally use it to filter out its
  // own announcements.
  origin;
  #quic;
  // The one writer for the outbound datagram stream (getWriter locks it), acquired once at
  // construction when this version + transport carry datagrams, released in close(). Its
  // presence is the gate: undefined means datagrams aren't served on this connection. All
  // subscriptions share it, since a second getWriter on the same stream would throw.
  #datagramWriter;
  // Our published broadcasts.
  // It's a signal so we can live update any announce streams.
  #broadcasts = new Signal(/* @__PURE__ */ new Map());
  // TRACK_INFO is immutable per track, so resolve it from the application once
  // (via a throwaway subscribe whose info() resolves when the app calls accept)
  // and reuse it for every later TRACK request of the same track. Keyed by
  // `broadcast\0track`. A rejected lookup is evicted so a retry can re-probe.
  #trackInfo = /* @__PURE__ */ new Map();
  /**
   * Creates a new Publisher instance.
   * @param quic - The WebTransport session to use
   * @param version - Negotiated protocol version
   * @param origin - Origin id shared with the Subscriber
   *
   * @internal
   */
  constructor(quic, version2, origin) {
    this.#quic = quic;
    this.version = version2;
    this.origin = origin;
    if (hasDatagrams(version2)) {
      this.#datagramWriter = datagramWriter(quic);
    }
  }
  /**
   * Publishes a broadcast with any associated tracks.
   * @param name - The broadcast to publish
   */
  publish(path, broadcast) {
    this.#broadcasts.mutate((broadcasts) => {
      if (!broadcasts) throw new Error("closed");
      broadcasts.set(path, broadcast);
    });
    void broadcast.closed.then(() => {
      this.#broadcasts.mutate((broadcasts) => {
        if (broadcasts?.get(path) === broadcast) {
          broadcasts.delete(path);
        }
      });
    });
  }
  /**
   * Handles an announce interest message.
   * @param msg - The announce interest message
   * @param stream - The stream to write announcements to
   *
   * @internal
   */
  async runAnnounce(msg, stream) {
    console.debug(`announce: prefix=${msg.prefix}`);
    let active = /* @__PURE__ */ new Map();
    let nextAnnounceId = 0n;
    const announceIds = /* @__PURE__ */ new Map();
    const announce = async (suffix, hops) => {
      console.debug(`announce: broadcast=${suffix} active=true`);
      if (hasAnnounceId(this.version)) announceIds.set(suffix, nextAnnounceId++);
      await encodeAnnounceBroadcast(stream.writer, { status: "active", suffix, hops }, this.version);
    };
    const retract = async (suffix) => {
      console.debug(`announce: broadcast=${suffix} active=false`);
      if (!hasAnnounceId(this.version)) {
        await encodeAnnounceBroadcast(stream.writer, { status: "ended", suffix }, this.version);
        return;
      }
      const id = announceIds.get(suffix);
      announceIds.delete(suffix);
      if (id === void 0) return;
      await encodeAnnounceBroadcast(stream.writer, { status: "endedId", id }, this.version);
    };
    let dispose;
    let changed = new Promise((resolve2) => {
      dispose = this.#broadcasts.changed(resolve2);
    });
    try {
      const initial = this.#broadcasts.peek();
      if (!initial) return;
      for (const [name, producer] of initial) {
        const suffix = stripPrefix(msg.prefix, name);
        if (suffix === null) continue;
        active.set(suffix, producer);
      }
      switch (this.version) {
        case Version2.DRAFT_01:
        case Version2.DRAFT_02: {
          for (const suffix of active.keys()) {
            console.debug(`announce: broadcast=${suffix} active=true`);
          }
          const init = new AnnounceInit([...active.keys()]);
          await init.encode(stream.writer, this.version);
          break;
        }
        default: {
          if (!hasAnnounceOk(this.version)) {
            for (const suffix of active.keys()) {
              await announce(suffix, [this.origin]);
            }
            break;
          }
          const ok = new AnnounceOk(this.origin, active.size);
          await ok.encode(stream.writer, this.version);
          for (const suffix of active.keys()) {
            await announce(suffix, []);
          }
          break;
        }
      }
      for (; ; ) {
        const broadcasts = await Promise.race([changed, stream.reader.closed]);
        dispose();
        if (!broadcasts) break;
        changed = new Promise((resolve2) => {
          dispose = this.#broadcasts.changed(resolve2);
        });
        const updated = /* @__PURE__ */ new Map();
        for (const [name, producer] of broadcasts) {
          const suffix = stripPrefix(msg.prefix, name);
          if (suffix === null) continue;
          updated.set(suffix, producer);
        }
        for (const [suffix, producer] of active) {
          if (updated.get(suffix) === producer) continue;
          await retract(suffix);
        }
        const hops = hasAnnounceOk(this.version) ? [] : [this.origin];
        for (const [suffix, producer] of updated) {
          if (active.get(suffix) === producer) continue;
          await announce(suffix, hops);
        }
        active = updated;
      }
    } finally {
      dispose();
    }
  }
  /**
   * Handles a subscribe message.
   * @param msg - The subscribe message
   * @param stream - The stream to write track data to
   *
   * @internal
   */
  async runSubscribe(msg, stream) {
    const broadcast = this.#broadcasts.peek()?.get(msg.broadcast);
    if (!broadcast) {
      console.debug(`publish unknown: broadcast=${msg.broadcast}`);
      stream.writer.reset(new Error("not found"));
      return;
    }
    const track = broadcast.subscribe(msg.track, {
      priority: msg.priority,
      ordered: msg.ordered,
      latencyMax: msg.maxLatency,
      startGroup: msg.startGroup,
      endGroup: msg.endGroup
    });
    const startGroup = msg.startGroup ?? track.latest();
    if (startGroup !== void 0) track.startAt(startGroup);
    track.endAt(msg.endGroup);
    let datagrams2 = Promise.resolve();
    try {
      let timescale = Timescale.MILLI;
      if (supportsTrackStream(this.version)) {
        const info = await track.info();
        timescale = info.timescale;
      } else {
        const ok = new SubscribeOk2({
          priority: msg.priority,
          ordered: msg.ordered,
          maxLatency: msg.maxLatency,
          startGroup: msg.startGroup,
          endGroup: msg.endGroup
        });
        await encodeSubscribeResponse(stream.writer, { ok }, this.version);
      }
      console.debug(`publish ok: broadcast=${msg.broadcast} track=${track.name}`);
      const serving = this.#runTrack(msg.id, msg.broadcast, track, stream.writer, timescale);
      if (this.#datagramWriter) {
        datagrams2 = this.#runDatagrams(msg.id, track, timescale);
      }
      for (; ; ) {
        const decode10 = SubscribeUpdate2.decodeMaybe(stream.reader, this.version);
        const result = await Promise.any([serving, decode10]);
        if (!result) break;
        if (result instanceof SubscribeUpdate2) {
          console.debug(`subscribe update: broadcast=${msg.broadcast} track=${track.name}`);
          track.update({
            priority: result.priority,
            ordered: result.ordered,
            latencyMax: result.maxLatency,
            startGroup: result.startGroup,
            endGroup: result.endGroup
          });
          if (result.startGroup !== void 0) track.startAt(result.startGroup);
          track.endAt(result.endGroup);
        }
      }
      console.debug(`publish done: broadcast=${msg.broadcast} track=${track.name}`);
      stream.close();
      track.close();
      await datagrams2;
    } catch (err) {
      const e = error(err);
      console.warn(`publish error: broadcast=${msg.broadcast} track=${track.name} error=${reason(e)}`);
      track.close(e);
      stream.abort(e);
      await datagrams2;
    }
  }
  /**
   * Handles a FETCH stream by serving one group as bare frame records (lite-05+).
   *
   * @internal
   */
  async runFetch(msg, stream) {
    if (!supportsTrackStream(this.version)) {
      stream.writer.reset(new Error("fetch requires moq-lite-05 or newer"));
      return;
    }
    const broadcast = this.#broadcasts.peek()?.get(msg.broadcast);
    if (!broadcast) {
      console.debug(`fetch unknown: broadcast=${msg.broadcast}`);
      stream.writer.reset(new Error("not found"));
      return;
    }
    stream.writer.setPriority(sendOrder({ priority: msg.priority }));
    let group;
    try {
      const info = await this.#resolveTrackInfo(msg.broadcast, msg.track);
      group = await broadcast.track(msg.track).fetchGroup(msg.group, { priority: msg.priority });
      await this.#runFetchGroup(group, stream.writer, Timescale(info.timescale));
      console.debug(`fetch done: broadcast=${msg.broadcast} track=${msg.track} group=${msg.group}`);
      stream.close();
      group.close();
    } catch (err) {
      const e = error(err);
      console.warn(
        `fetch error: broadcast=${msg.broadcast} track=${msg.track} group=${msg.group} error=${reason(e)}`
      );
      group?.close(e);
      stream.abort(e);
    }
  }
  /**
   * Runs a track and sends its data to the stream.
   * @param sub - The subscription ID
   * @param broadcast - The broadcast name
   * @param track - The track to run
   * @param stream - The stream to write to
   *
   * @internal
   */
  async #runTrack(sub, broadcast, track, stream, timescale) {
    const emitRange = supportsTrackStream(this.version);
    let startSent = false;
    let end = 0;
    const priority = new Priority(track);
    let finished = false;
    let unsubscribe;
    const unsubscribed = new Promise((resolve2) => {
      unsubscribe = resolve2;
    });
    void stream.closed.then(
      () => {
        if (!finished) unsubscribe();
      },
      // A reset is always the peer.
      () => unsubscribe()
    );
    try {
      for (; ; ) {
        const next = track.recvGroup();
        const group = await Promise.race([next, stream.closed]);
        if (!group) {
          next.then((group2) => group2?.close()).catch(() => {
          });
          break;
        }
        if (emitRange && !startSent) {
          startSent = true;
          track.startAt(group.sequence);
          await encodeSubscribeResponse(stream, { start: new SubscribeStart(group.sequence) }, this.version);
        }
        end = Math.max(end, group.sequence + 1);
        void this.#runGroup({ sub, group, timescale, priority, unsubscribed });
      }
      if (emitRange) {
        await encodeSubscribeResponse(stream, { end: new SubscribeEnd(end) }, this.version);
      }
      console.debug(`publish close: broadcast=${broadcast} track=${track.name}`);
      finished = true;
      track.close();
      stream.close();
    } catch (err) {
      const e = error(err);
      console.warn(`publish error: broadcast=${broadcast} track=${track.name} error=${reason(e)}`);
      unsubscribe();
      track.close(e);
      stream.reset(e);
    } finally {
      priority.close();
    }
  }
  /**
   * Answers a TRACK stream (0x6) with a single TRACK_INFO, then FINs.
   *
   * @internal
   */
  async runTrackInfo(msg, stream) {
    try {
      const info = await this.#resolveTrackInfo(msg.broadcast, msg.track);
      await info.encode(stream.writer, this.version);
      console.debug(`track info: broadcast=${msg.broadcast} track=${msg.track}`);
      stream.close();
    } catch (err) {
      console.debug(`track unknown: broadcast=${msg.broadcast} track=${msg.track}`);
      stream.writer.reset(error(err));
    }
  }
  // Resolve (and cache) a track's immutable TRACK_INFO by asking the application.
  // `broadcast.track(name).info()` triggers a TrackRequest the app answers with
  // accept(TrackInfo); only the immutable properties are needed (not the groups).
  // Cached because they're fixed for the track's lifetime. Rejects if the broadcast
  // or track is unavailable.
  #resolveTrackInfo(broadcast, track) {
    const key = `${broadcast}\0${track}`;
    const cached2 = this.#trackInfo.get(key);
    if (cached2) return cached2;
    const pending = (async () => {
      const published = this.#broadcasts.peek()?.get(broadcast);
      if (!published) throw new Error("not found");
      const info = await published.track(track).info();
      return new TrackInfo({
        priority: info.priority,
        ordered: info.ordered,
        // Publisher Max Latency: the publisher's retention bound, advertised so
        // relays re-serve with the same window.
        latencyMax: info.latencyMax,
        // Lite05 mandates per-frame timestamps. Advertise the track's timescale;
        // `#runGroup` emits each frame converted to it.
        timescale: info.timescale
      });
    })();
    pending.catch(() => this.#trackInfo.delete(key));
    this.#trackInfo.set(key, pending);
    return pending;
  }
  /**
   * Forwards a track's datagrams best-effort over QUIC datagrams (lite-05 §6.4), parallel to
   * its groups. Each datagram is dropped (there is no group fallback) if the encoded body
   * doesn't fit the transport's datagram limit or the send fails. Returns once the track
   * finishes; a failure never tears down the subscription.
   *
   * @internal
   */
  async #runDatagrams(sub, track, timescale) {
    const writer = this.#datagramWriter;
    if (!writer) return;
    const maxSize = maxDatagramSize(this.#quic);
    try {
      for (; ; ) {
        const datagram = await track.recvDatagram();
        if (!datagram) return;
        const ts = Math.round(datagram.timestamp.as(timescale));
        const body = new Datagram(sub, datagram.sequence, ts, datagram.payload).encode();
        if (body.byteLength > maxSize) {
          console.debug(`dropping oversize datagram: sub=${sub} size=${body.byteLength} max=${maxSize}`);
          continue;
        }
        await writer.ready;
        await writer.write(body);
      }
    } catch (err) {
      console.debug(`datagram send stopped: sub=${sub} error=${reason(err)}`);
    }
  }
  // Serialize a fetched group's frames onto the FETCH stream as bare records: each a
  // zigzag-delta timestamp (at the track's advertised timescale) followed by size + bytes.
  async #runFetchGroup(group, stream, timescale) {
    let prevTs = 0n;
    for (; ; ) {
      const frame = await Promise.race([group.readFrame(), stream.closed]);
      if (!frame) break;
      const ts = BigInt(Math.round(frame.timestamp.as(timescale)));
      await stream.u62(zigzag(ts - prevTs));
      prevTs = ts;
      await stream.u53(frame.payload.byteLength);
      await stream.write(frame.payload);
    }
  }
  /**
   * Serves one group on its own unidirectional stream.
   *
   * @internal
   */
  async #runGroup(options) {
    const { sub, group, timescale, priority, unsubscribed } = options;
    const msg = new Group2(sub, group.sequence);
    try {
      const stream = await Writer.tryOpen(this.#quic, {
        sendOrder: priority.rank(group.sequence),
        cancel: unsubscribed,
        waitUntilAvailable: false
      });
      if (!stream) {
        group.close(new Error("no stream slot"));
        return;
      }
      try {
        priority.add(stream, group.sequence);
        await stream.u53(0);
        await msg.encode(stream);
        const timestamps = supportsTrackStream(this.version);
        let prevTs = 0n;
        for (; ; ) {
          const frame = await Promise.race([group.readFrame(), stream.closed]);
          if (!frame) break;
          if (timestamps) {
            const ts = BigInt(Math.round(frame.timestamp.as(timescale)));
            await stream.u62(zigzag(ts - prevTs));
            prevTs = ts;
          }
          await stream.u53(frame.payload.byteLength);
          await stream.write(frame.payload);
        }
        stream.close();
        group.close();
      } catch (err) {
        const e = error(err);
        stream.reset(e);
        group.close(e);
      } finally {
        priority.remove(stream);
      }
    } catch (err) {
      const e = error(err);
      group.close(e);
    }
  }
  /**
   * Handles a probe stream by periodically reporting estimated bitrate.
   * @param stream - The probe bidi stream
   *
   * @internal
   */
  async runProbe(stream) {
    const quic = this.#quic;
    if (!quic.getStats) {
      stream.close();
      return;
    }
    let lastSent;
    let lastSentTime;
    const moved = (prev, next, threshold = 0) => {
      if (prev === void 0 && next === void 0) return false;
      if (prev === void 0 || next === void 0) return true;
      if (prev === 0) return next !== 0;
      return Math.abs(next - prev) / prev >= threshold;
    };
    try {
      for (; ; ) {
        const timeout = new Promise(
          (resolve2) => setTimeout(() => resolve2("timeout"), PROBE_INTERVAL)
        );
        const result = await Promise.race([timeout, stream.reader.closed]);
        if (result !== "timeout") break;
        const stats = await quic.getStats();
        const rtt = stats.smoothedRtt != null ? Math.round(stats.smoothedRtt) : void 0;
        const report = new Probe({
          bitrate: stats.estimatedSendRate ?? void 0,
          rtt: hasProbeRtt(this.version) ? rtt : void 0
        });
        if (report.bitrate === void 0 && report.rtt === void 0) {
          const retracts = lastSent !== void 0 && (lastSent.bitrate !== void 0 || lastSent.rtt !== void 0);
          if (!retracts) continue;
        }
        let shouldSend;
        if (lastSent === void 0 || lastSentTime === void 0) {
          shouldSend = true;
        } else {
          const elapsed = performance.now() - lastSentTime;
          const t = Math.max(PROBE_INTERVAL, Math.min(PROBE_MAX_AGE, elapsed));
          const range = PROBE_MAX_AGE - PROBE_INTERVAL;
          const threshold = PROBE_MAX_DELTA * (PROBE_MAX_AGE - t) / range;
          shouldSend = elapsed >= PROBE_MAX_AGE || moved(lastSent.bitrate, report.bitrate, threshold) || moved(lastSent.rtt, report.rtt, PROBE_RTT_DELTA);
        }
        if (shouldSend) {
          await report.encode(stream.writer, this.version);
          lastSent = report;
          lastSentTime = performance.now();
        }
      }
    } catch (err) {
      console.warn("probe stream error", err);
      stream.close();
    }
  }
  close() {
    this.#broadcasts.update((broadcasts) => {
      for (const broadcast of broadcasts?.values() ?? []) {
        broadcast.close();
      }
      return void 0;
    });
    this.#datagramWriter?.releaseLock();
    this.#datagramWriter = void 0;
  }
};

// vendor/moq/js/net/src/lite/session.ts
var SessionInfo = class _SessionInfo {
  bitrate;
  constructor(bitrate) {
    this.bitrate = bitrate;
  }
  static #guard(version2) {
    switch (version2) {
      case Version2.DRAFT_01:
      case Version2.DRAFT_02:
        break;
      default:
        throw new Error("session info not supported for this version");
    }
  }
  async #encode(w) {
    await w.u53(this.bitrate);
  }
  static async #decode(r) {
    const bitrate = await r.u53();
    return new _SessionInfo(bitrate);
  }
  async encode(w, version2) {
    _SessionInfo.#guard(version2);
    return encode9(w, this.#encode.bind(this));
  }
  static async decode(r, version2) {
    _SessionInfo.#guard(version2);
    return decode9(r, _SessionInfo.#decode);
  }
  static async decodeMaybe(r, version2) {
    _SessionInfo.#guard(version2);
    return decodeMaybe(r, _SessionInfo.#decode);
  }
};

// vendor/moq/js/net/src/lite/setup.ts
var PARAM_PROBE = 0x1n;
var PARAM_PATH = 0x2n;
var PARAM_ROLE = 0x3n;
var PARAM_ORIGIN = 0x5n;
var MAX_PARAMS = 64;
var ProbeLevel = {
  /** No probing. Equivalent to omitting the parameter. */
  None: 0,
  /** The publisher can measure and periodically report its estimated bitrate. */
  Report: 1,
  /** The publisher can additionally pad the connection (or send redundant data). */
  Increase: 2
};
function probeFromCode(code) {
  switch (code) {
    case 0n:
      return ProbeLevel.None;
    case 1n:
      return ProbeLevel.Report;
    default:
      return ProbeLevel.Increase;
  }
}
var Role = {
  /** The client may do either, or declined to say. Equivalent to omitting the parameter. */
  Both: 0,
  /** The client will publish tracks (ingest); the server must consume. */
  Publisher: 1,
  /** The client will subscribe to tracks (egress); the server must publish. */
  Subscriber: 2
};
function roleFromCode(code) {
  switch (code) {
    case 1n:
      return Role.Publisher;
    case 2n:
      return Role.Subscriber;
    default:
      return Role.Both;
  }
}
var Parameters2 = class _Parameters {
  #entries = /* @__PURE__ */ new Map();
  /** Set a parameter to a raw byte value, replacing any existing entry. */
  setBytes(id, value) {
    this.#entries.set(id, value);
  }
  /** Return a parameter's raw byte value, if present. */
  getBytes(id) {
    return this.#entries.get(id);
  }
  /** Set a parameter to a varint value, replacing any existing entry. */
  setVarint(id, value) {
    this.#entries.set(id, encode2(Number(value)));
  }
  /** Decode a parameter as a single varint, if present. Throws if trailing bytes remain. */
  getVarint(id) {
    const bytes = this.#entries.get(id);
    if (bytes === void 0) return void 0;
    const [value, remain] = decode2(bytes);
    if (remain.byteLength !== 0) {
      throw new Error("trailing bytes after varint parameter");
    }
    return BigInt(value);
  }
  async encode(w) {
    if (this.#entries.size > MAX_PARAMS) {
      throw new Error("too many parameters");
    }
    await w.u53(this.#entries.size);
    for (const [id, value] of this.#entries) {
      await w.u62(id);
      await w.u53(value.byteLength);
      await w.write(value);
    }
  }
  static async decode(r) {
    const params = new _Parameters();
    const count = await r.u53();
    if (count > MAX_PARAMS) {
      throw new Error("too many parameters");
    }
    for (let i = 0; i < count; i++) {
      const id = await r.u62();
      if (params.#entries.has(id)) {
        throw new Error(`duplicate parameter id: ${id.toString()}`);
      }
      const size2 = await r.u53();
      const value = await r.read(size2);
      params.#entries.set(id, value);
    }
    return params;
  }
};
var Setup2 = class _Setup {
  /** The probe capability this endpoint supports. {@link ProbeLevel.None} when absent. */
  probe;
  /**
   * The request path, for transports that carry no request URI (native QUIC, qmux over
   * TCP/TLS, unix sockets), with `?` and the URI query appended when there is one. Sent
   * only by the client; a server never sends one and a relay never forwards it.
   * `undefined` on URI-carrying bindings such as WebTransport, where sending one is a
   * protocol violation. An empty string means the same as `undefined`.
   */
  path;
  /**
   * The client's intended {@link Role}. `Both` is sent as the absence of the parameter, so
   * a client that never sets it decodes back to `Both`. Sent only by the client; a server
   * never sends one and a relay never forwards it.
   */
  role;
  /**
   * This endpoint's origin (hop) id. The peer uses it to filter announcements and
   * subscriptions whose route flows through this endpoint (lite-06 removed the
   * per-stream `exclude_hop` in its favor). `undefined` when the endpoint declares
   * no identity; a wire value of 0 decodes the same way.
   */
  origin;
  constructor({ probe, path, role, origin } = {}) {
    this.probe = probe ?? ProbeLevel.None;
    this.path = path;
    this.role = role ?? Role.Both;
    this.origin = origin;
  }
  static #guard(version2) {
    if (!hasSetupStream(version2)) {
      throw new Error("setup stream not supported for this version");
    }
  }
  async #encode(w) {
    const params = new Parameters2();
    if (this.probe !== ProbeLevel.None) {
      params.setVarint(PARAM_PROBE, this.probe);
    }
    if (this.path !== void 0) {
      params.setBytes(PARAM_PATH, new TextEncoder().encode(this.path));
    }
    if (this.role !== Role.Both) {
      params.setVarint(PARAM_ROLE, this.role);
    }
    if (this.origin !== void 0 && this.origin !== 0n) {
      params.setVarint(PARAM_ORIGIN, this.origin);
    }
    await params.encode(w);
  }
  static async #decode(r) {
    const params = await Parameters2.decode(r);
    const probeCode = params.getVarint(PARAM_PROBE);
    const probe = probeCode === void 0 ? ProbeLevel.None : probeFromCode(probeCode);
    const pathBytes = params.getBytes(PARAM_PATH);
    const path = pathBytes === void 0 ? void 0 : decodeUtf8(pathBytes);
    const roleCode = params.getVarint(PARAM_ROLE);
    const role = roleCode === void 0 ? Role.Both : roleFromCode(roleCode);
    const originRaw = params.getVarint(PARAM_ORIGIN);
    const origin = originRaw === void 0 || originRaw === 0n ? void 0 : OriginSchema.parse(originRaw);
    return new _Setup({ probe, path, role, origin });
  }
  /** Encode the SETUP message with its size prefix. Throws on pre-lite-05 versions. */
  async encode(w, version2) {
    _Setup.#guard(version2);
    return encode9(w, this.#encode.bind(this));
  }
  /** Decode a SETUP message with its size prefix. Throws on pre-lite-05 versions. */
  static async decode(r, version2) {
    _Setup.#guard(version2);
    return decode9(r, _Setup.#decode);
  }
};

// vendor/moq/js/net/src/lite/stream.ts
var StreamId = {
  Session: 0,
  Announce: 1,
  Subscribe: 2,
  Fetch: 3,
  Probe: 4,
  Goaway: 5,
  Track: 6,
  ClientCompat: 32,
  ServerCompat: 33
};
var DataType = {
  Group: 0,
  Setup: 1
};

// vendor/moq/js/net/src/lite/subscriber.ts
var SUBSCRIBE_SETUP_TIMEOUT_MS = 1e4;
function unzigzag(v) {
  return v >> 1n ^ -(v & 1n);
}
function supportsTrackStream2(version2) {
  switch (version2) {
    case Version2.DRAFT_01:
    case Version2.DRAFT_02:
    case Version2.DRAFT_03:
    case Version2.DRAFT_04:
      return false;
    default:
      return true;
  }
}
var PROTOCOL_VIOLATION_CODE = 15;
var MAX_CLOSE_REASON = 1024;
function closeReason(text) {
  const encoder = new TextEncoder();
  const buf = new Uint8Array(MAX_CLOSE_REASON);
  const { read } = encoder.encodeInto(text, buf);
  return text.slice(0, read);
}
var Subscriber3 = class {
  #quic;
  // The version of the connection.
  version;
  // Shared with the Publisher so callers can optionally filter out their
  // own announcements on a per-call basis (see {@link AnnouncedOptions}).
  origin;
  // Our subscribed tracks. `timescale` resolves once known (from TRACK_INFO on
  // lite-05+, or implicit defaults on older drafts); group streams block on it
  // before decoding any frame, since a group's QUIC stream can race ahead.
  #subscribes = /* @__PURE__ */ new Map();
  #subscribeNext = 0n;
  // Dedup consumed broadcasts per path: repeat consume() calls share one subscription.
  #consumes = new BroadcastCache();
  // Dedup in-flight one-shot fetches, keyed by [broadcast, track, sequence]. Concurrent (or
  // repeat, while still open) fetchGroup() calls for the same group share one FETCH stream and
  // each get an independent mirror; the entry is evicted once the group closes.
  #fetches = /* @__PURE__ */ new Map();
  // The peer's PROBE estimates, written as they arrive (Lite03+ only).
  #probe;
  // The peer's SETUP (lite-05+), undefined until it arrives. Gates opening the PROBE
  // stream on the peer having advertised Probe >= Report.
  #peerSetup;
  // Distinguishes failures from streams torn down by Subscriber.close().
  #closed = new AbortController();
  /**
   * Creates a new Subscriber instance.
   * @param quic - The WebTransport session to use
   * @param version - The protocol version
   * @param origin - Origin id shared with the Publisher
   * @param probe - Optional sink for the peer's PROBE estimates
   * @param peerSetup - Optional peer SETUP slot for capability gating (lite-05+)
   *
   * @internal
   */
  constructor(quic, version2, origin, probe, peerSetup) {
    this.#quic = quic;
    this.version = version2;
    this.origin = origin;
    this.#probe = probe;
    this.#peerSetup = peerSetup;
  }
  /**
   * Subscribe to broadcast announcements under `prefix`.
   *
   * Pass `{ ignoreSelf: true }` to skip announces that have already traversed
   * this connection's {@link origin}.
   */
  announced(prefix = empty(), options = {}) {
    const announced = new Producer(prefix);
    void this.#runAnnounced(announced, prefix, options);
    return announced.consume();
  }
  async #runAnnounced(announced, prefix, options) {
    console.debug(`announced: prefix=${prefix}`);
    const msg = new AnnounceRequest(prefix, this.origin);
    const dropReflected = options.ignoreSelf || !hasExcludeHop(this.version);
    let stream;
    try {
      stream = await Stream.open(this.#quic);
    } catch (err) {
      announced.close(error(err));
      return;
    }
    try {
      await stream.writer.u53(StreamId.Announce);
      await msg.encode(stream.writer, this.version);
      let responderOrigin;
      if (hasAnnounceOk(this.version)) {
        const ok = await AnnounceOk.decode(stream.reader, this.version);
        responderOrigin = ok.origin === UNKNOWN_ORIGIN ? void 0 : ok.origin;
      }
      const advertised = /* @__PURE__ */ new Map();
      switch (this.version) {
        case Version2.DRAFT_01:
        case Version2.DRAFT_02: {
          const init = await AnnounceInit.decode(stream.reader, this.version);
          for (const suffix of init.suffixes) {
            const path = join(prefix, suffix);
            if (advertised.has(suffix)) {
              throw new ProtocolViolation(`duplicate announce for ${path}`);
            }
            advertised.set(suffix, { publisher: void 0, live: true });
            console.debug(`announced: broadcast=${path} active=true`);
            announced.append({ path: suffix, active: true });
          }
          break;
        }
        default:
          break;
      }
      let nextAnnounceId = 0n;
      const announcedById = /* @__PURE__ */ new Map();
      for (; ; ) {
        const announce = await Promise.race([
          decodeAnnounceBroadcastMaybe(stream.reader, this.version),
          announced.closed
        ]);
        if (!announce) break;
        if (announce instanceof Error) throw announce;
        let suffix;
        let active;
        let hops;
        switch (announce.status) {
          case "active":
            suffix = announce.suffix;
            active = true;
            hops = announce.hops;
            if (hasAnnounceId(this.version)) {
              announcedById.set(nextAnnounceId++, announce.suffix);
            }
            break;
          case "ended":
            suffix = announce.suffix;
            active = false;
            break;
          case "endedId": {
            const path2 = announcedById.get(announce.id);
            if (path2 === void 0) throw new ProtocolViolation(`unknown announce id: ${announce.id}`);
            announcedById.delete(announce.id);
            suffix = path2;
            active = false;
            break;
          }
          case "restart": {
            const path2 = announcedById.get(announce.id);
            if (path2 === void 0) throw new ProtocolViolation(`unknown announce id: ${announce.id}`);
            suffix = path2;
            active = true;
            hops = announce.hops;
            break;
          }
        }
        const path = join(prefix, suffix);
        const duplicateIsRestart = restartSupported(this.version) && !hasAnnounceId(this.version);
        if (announce.status === "active" && !duplicateIsRestart && advertised.has(suffix)) {
          throw new ProtocolViolation(`duplicate announce for ${path}`);
        }
        const retract = () => {
          const previous = advertised.get(suffix);
          advertised.delete(suffix);
          if (!previous?.live) return;
          this.#consumes.evict(path);
          console.debug(`announced: broadcast=${path} active=false`);
          announced.append({ path: suffix, active: false });
        };
        if (hops !== void 0 && dropReflected) {
          const full = responderOrigin !== void 0 ? [...hops, responderOrigin] : hops;
          if (full.includes(this.origin)) {
            retract();
            advertised.set(suffix, { publisher: void 0, live: false });
            continue;
          }
        }
        if (active) {
          const publisher = hops?.[0] ?? responderOrigin;
          const identified = publisher !== void 0 && publisher !== UNKNOWN_ORIGIN;
          const previous = advertised.get(suffix);
          if (previous?.live) {
            if (identified && previous.publisher === publisher) {
              console.debug(`announced: broadcast=${path} rerouted`);
              continue;
            }
            retract();
          }
          advertised.set(suffix, { publisher, live: true });
        } else {
          retract();
          continue;
        }
        console.debug(`announced: broadcast=${path} active=true`);
        announced.append({ path: suffix, active: true });
      }
      announced.close();
    } catch (err) {
      const e = error(err);
      stream.abort(e);
      announced.close(e);
      if (e instanceof ProtocolViolation) {
        this.#quic.close({ closeCode: PROTOCOL_VIOLATION_CODE, reason: closeReason(reason(e)) });
      }
    }
  }
  /**
   * Consumes a broadcast from the connection.
   *
   * Deduplicated per path: repeat calls for the same still-live path share one reference-counted
   * broadcast (and one upstream subscription). The shared broadcast closes once every caller has
   * closed its handle, so callers close normally.
   *
   * @param name - The name of the broadcast to consume
   * @returns A Broadcast instance
   */
  consume(path) {
    return this.#consumes.get(path) ?? this.#consumes.insert(path, this.#createConsume(path));
  }
  #createConsume(path) {
    const consumer = new ConsumeBroadcast2(this, path);
    void (async () => {
      for (; ; ) {
        const request = await consumer.requested();
        if (!request) break;
        void this.#runSubscribe(path, request);
      }
    })();
    return consumer;
  }
  async #runSubscribe(broadcast, request) {
    const id = this.#subscribeNext++;
    const subscription = request.subscription;
    const timescale = new Signal(void 0);
    console.debug(`subscribe start: id=${id} broadcast=${broadcast} track=${request.name}`);
    const msg = new Subscribe2({
      id,
      broadcast,
      track: request.name,
      priority: subscription.priority ?? 0,
      ordered: subscription.ordered,
      maxLatency: subscription.latencyMax,
      startGroup: subscription.startGroup,
      endGroup: subscription.endGroup
    });
    const state = {};
    const setup = this.#openSubscribe(state, msg, request, id, timescale);
    let opened;
    try {
      opened = await withTimeout(
        setup,
        SUBSCRIBE_SETUP_TIMEOUT_MS,
        `subscribe timed out after ${SUBSCRIBE_SETUP_TIMEOUT_MS}ms waiting for the first response (browser stream limit reached?)`
      );
      console.debug(`subscribe ok: id=${id} broadcast=${broadcast} track=${request.name}`);
    } catch (err) {
      const e = error(err);
      request.reject(e);
      this.#subscribes.delete(id);
      console.warn(`subscribe error: id=${id} broadcast=${broadcast} track=${request.name} error=${reason(e)}`);
      setup.then(
        () => state.stream?.abort(e),
        () => state.stream?.abort(e)
      );
      return;
    }
    const { stream, producer } = opened;
    try {
      const closed = supportsTrackStream2(this.version) ? this.#drainResponses(stream) : stream.reader.closed;
      const subscriptionUpdates = this.version === Version2.DRAFT_01 || this.version === Version2.DRAFT_02 ? void 0 : this.#runSubscriptionUpdates(id, broadcast, producer, msg, stream);
      const terminal = [closed, producer.closed];
      if (subscriptionUpdates !== void 0) terminal.push(subscriptionUpdates);
      const done = Promise.race(terminal);
      const idle = /* @__PURE__ */ Symbol("idle");
      for (; ; ) {
        const reason2 = await Promise.race([done, producer.unused().then(() => idle)]);
        if (reason2 === idle && producer.closed.peek() === void 0 && producer.used.peek()) continue;
        break;
      }
      producer.close();
      stream.close();
      console.debug(`subscribe close: id=${id} broadcast=${broadcast} track=${request.name}`);
    } catch (err) {
      const e = error(err);
      producer.close(e);
      console.warn(`subscribe error: id=${id} broadcast=${broadcast} track=${request.name} error=${reason(e)}`);
      stream.abort(e);
    } finally {
      this.#subscribes.delete(id);
    }
  }
  // Determine the track's immutable properties, accept the request (so the
  // application's track.Subscriber resolves and incoming groups have a producer to
  // write into), register it, then open the subscribe stream. `state.stream` is
  // populated as soon as the subscribe stream opens so the caller can clean it up
  // on timeout even before this promise settles.
  //
  // On lite-05+ the properties come from a TRACK stream opened first, and the
  // SUBSCRIBE is accepted implicitly (no SUBSCRIBE_OK). Older drafts carry no
  // per-track properties, so they resolve to defaults and just drain SUBSCRIBE_OK.
  async #openSubscribe(state, msg, request, id, timescale) {
    let producer;
    let drainOk = false;
    if (supportsTrackStream2(this.version)) {
      const info = await this.#trackInfo(msg.broadcast, msg.track);
      producer = request.accept(this.#toModelInfo(info));
      timescale.set(info.timescale);
    } else {
      producer = request.accept();
      timescale.set(0);
      drainOk = true;
    }
    this.#subscribes.set(id, { track: producer, timescale });
    state.stream = await Stream.open(this.#quic);
    await state.stream.writer.u53(StreamId.Subscribe);
    await msg.encode(state.stream.writer, this.version);
    if (drainOk) {
      const resp = await decodeSubscribeResponse(state.stream.reader, this.version);
      if (!("ok" in resp)) {
        throw new Error("first subscribe response must be SUBSCRIBE_OK");
      }
    }
    return { stream: state.stream, producer };
  }
  // Opens a TRACK stream, reads the single TRACK_INFO, and FINs. Lite-05+ only.
  async #trackInfo(broadcast, track) {
    const stream = await Stream.open(this.#quic);
    try {
      await stream.writer.u53(StreamId.Track);
      await new Track(broadcast, track).encode(stream.writer, this.version);
      const info = await TrackInfo.decode(stream.reader, this.version);
      stream.close();
      return info;
    } catch (err) {
      stream.abort(error(err));
      throw err;
    }
  }
  // Map the wire TRACK_INFO onto the model track.Info a producer/consumer holds.
  #toModelInfo(info) {
    return {
      timescale: Timescale(info.timescale),
      // Publisher Max Latency rides on the wire, so the local retention window
      // matches what the upstream advertises (relays re-serve with the same bound).
      latencyMax: info.latencyMax,
      priority: info.priority,
      ordered: info.ordered
    };
  }
  // Resolve a track's immutable model info via a TRACK stream (lite-05+), for the
  // ConsumeBroadcast backing track.Consumer.info(). On older drafts there's no TRACK
  // stream, so this rejects rather than fabricating defaults.
  async resolveTrackInfo(broadcast, track) {
    if (!supportsTrackStream2(this.version)) {
      throw new Error("track info requires moq-lite-05 or newer");
    }
    return this.#toModelInfo(await this.#trackInfo(broadcast, track));
  }
  // Open a FETCH stream for one group and stream its bare frames into a group, for the
  // ConsumeBroadcast backing track.Consumer.fetchGroup() (lite-05+).
  fetchGroup(broadcast, track, sequence, options = {}) {
    const key = JSON.stringify([broadcast, track, sequence]);
    const existing = this.#fetches.get(key);
    if (existing && !existing.isClosed) return Promise.resolve(existing.mirror());
    const group = new Producer2(sequence);
    this.#fetches.set(key, group);
    void group.closed.then(() => {
      if (this.#fetches.get(key) === group) this.#fetches.delete(key);
    });
    return this.#runFetch(broadcast, track, sequence, options, group);
  }
  // Open the FETCH stream and pump the response into the shared group. Setup errors close the
  // group (so coalesced mirrors observe them and the entry evicts) and reject this caller.
  async #runFetch(broadcast, track, sequence, options, group) {
    try {
      if (!supportsTrackStream2(this.version)) {
        throw new Error("fetch group requires moq-lite-05 or newer");
      }
      const info = await this.#trackInfo(broadcast, track);
      const priority = options.priority ?? 0;
      const stream = await Stream.open(this.#quic, { sendOrder: sendOrder({ priority }) });
      try {
        await stream.writer.u53(StreamId.Fetch);
        await new Fetch(broadcast, track, priority, sequence).encode(stream.writer, this.version);
      } catch (err) {
        stream.abort(error(err));
        throw err;
      }
      const consumer = group.mirror();
      void this.#runFetchResponse(stream, group, Timescale(info.timescale));
      return consumer;
    } catch (err) {
      group.close(error(err));
      throw err;
    }
  }
  // Read the FETCH response (bare zigzag-delta-timestamped frames) into the group, then
  // FIN. A stream-level failure aborts the group so its reader observes the gap.
  async #runFetchResponse(stream, group, timescale) {
    try {
      let prevTs = 0n;
      const idle = /* @__PURE__ */ Symbol("idle");
      const closed = Promise.resolve(group.closed);
      let unused = group.unused().then(() => idle);
      for (; ; ) {
        const done = await Promise.race([stream.reader.done(), closed, unused]);
        if (done === idle) {
          if (!group.isClosed && group.used.peek()) {
            unused = group.unused().then(() => idle);
            continue;
          }
          break;
        }
        if (done !== false) break;
        prevTs += unzigzag(await stream.reader.u62());
        const timestamp = new Timestamp(Number(prevTs), timescale);
        const size2 = await stream.reader.u53();
        const payload = await stream.reader.read(size2);
        if (!payload) break;
        group.writeFrame({ payload, timestamp });
      }
      group.close();
      stream.close();
    } catch (err) {
      const e = error(err);
      group.close(e);
      stream.abort(e);
    }
  }
  // Drains SUBSCRIBE_START/END/DROP on the subscribe stream until FIN (lite-05+).
  // The resolved range is informational here; the producer already orders groups.
  // Resolves (never rejects) on FIN or on the stream being reset out from under it,
  // so it's safe to drop from a Promise.race without an unhandled rejection.
  async #drainResponses(stream) {
    try {
      for (; ; ) {
        const resp = await decodeSubscribeResponseMaybe(stream.reader, this.version);
        if (!resp) return;
      }
    } catch {
    }
  }
  /**
   * Send SUBSCRIBE_UPDATE messages whenever the track's aggregate subscription changes.
   *
   * Resolves cleanly when the stream or track closes, so the caller can include
   * this in Promise.race without leaving a dangling pending write that would
   * become an unhandled rejection if the user calls update after close.
   *
   * Peeks the signal at the top of every iteration so that updates which landed
   * before SubscribeOk arrived (or between iterations, before .next() registered
   * its listener) aren't lost.
   */
  async #runSubscriptionUpdates(id, broadcast, track, msg, stream) {
    const stopped = Promise.race([track.closed, stream.reader.closed]).then(() => null);
    let lastSent = {
      priority: msg.priority,
      ordered: msg.ordered,
      latencyMax: msg.maxLatency,
      startGroup: msg.startGroup,
      endGroup: msg.endGroup
    };
    for (; ; ) {
      const current = track.subscription.peek();
      if (current === void 0 || this.#sameSubscription(current, lastSent)) {
        const next = await Promise.race([track.subscription.changed(), stopped]);
        if (next === null) return;
        continue;
      }
      const update = new SubscribeUpdate2({
        priority: current.priority ?? 0,
        ordered: current.ordered,
        maxLatency: current.latencyMax,
        startGroup: current.startGroup,
        endGroup: current.endGroup
      });
      await update.encode(stream.writer, this.version);
      lastSent = { ...current };
      console.debug(`subscribe update: id=${id} broadcast=${broadcast} track=${track.name}`);
    }
  }
  #sameSubscription(a, b) {
    return (a.priority ?? 0) === (b.priority ?? 0) && (a.ordered ?? false) === (b.ordered ?? false) && (a.latencyMax ?? 0) === (b.latencyMax ?? 0) && a.startGroup === b.startGroup && a.endGroup === b.endGroup;
  }
  /**
   * Handles a group message.
   * @param group - The group message
   * @param stream - The stream to read frames from
   *
   * @internal
   */
  async runGroup(group, stream) {
    const entry = this.#subscribes.get(group.subscribe);
    if (!entry) {
      if (group.subscribe >= this.#subscribeNext) {
        throw new Error(`unknown subscription: id=${group.subscribe}`);
      }
      return;
    }
    const { track, timescale } = entry;
    const producer = new Producer2(group.sequence);
    track.writeGroup(producer);
    try {
      let scale = timescale.peek();
      while (scale === void 0) {
        if (track.closed.peek() !== void 0) {
          producer.close();
          stream.stop(new Error("cancel"));
          return;
        }
        await Signal.race(timescale, track.closed);
        scale = timescale.peek();
      }
      let prevTs = 0n;
      for (; ; ) {
        const done = await Promise.race([stream.done(), track.closed, producer.closed]);
        if (done !== false) break;
        let timestamp;
        if (scale !== 0) {
          prevTs += unzigzag(await stream.u62());
          timestamp = new Timestamp(Number(prevTs), Timescale(scale));
        } else {
          timestamp = Timestamp.now();
        }
        const size2 = await stream.u53();
        const payload = await stream.read(size2);
        if (!payload) break;
        producer.writeFrame({ payload, timestamp });
      }
      producer.close();
      stream.stop(new Error("cancel"));
    } catch (err) {
      const e = error(err);
      producer.close(e);
      stream.stop(e);
    }
  }
  /**
   * Receives QUIC datagrams and routes each to its subscription's track producer (lite-05 §6.4).
   *
   * Returns immediately on a non-datagram transport or pre-lite-05 version. A decode error or an
   * unknown subscribe id drops that datagram without tearing down the session (best-effort); the
   * loop ends only when the datagram stream closes.
   *
   * @internal
   */
  async runDatagrams() {
    if (!hasDatagrams(this.version) || maxDatagramSize(this.#quic) === 0) {
      return;
    }
    const reader = datagramReader(this.#quic);
    if (!reader) return;
    try {
      try {
        for (; ; ) {
          const { value, done } = await reader.read();
          if (done) break;
          if (!value) continue;
          try {
            await this.#routeDatagram(value);
          } catch (err) {
            console.debug(`dropping datagram: ${reason(err)}`);
          }
        }
      } finally {
        reader.releaseLock();
      }
    } catch (err) {
      const e = error(err);
      if (e.message === "The session is closed.") {
        console.debug(`datagram receive stopped: ${e.message}`);
      } else {
        console.warn("datagram stream error", err);
      }
    }
  }
  // Decode one datagram body and hand it to the matching subscription's producer. Drops the
  // datagram (best-effort) if the subscription is unknown/closed or its timescale isn't resolved.
  async #routeDatagram(payload) {
    const dg = await Datagram.decode(payload);
    const entry = this.#subscribes.get(dg.subscribe);
    if (!entry) return;
    const scale = entry.timescale.peek();
    if (!scale) return;
    const timestamp = new Timestamp(dg.timestamp, Timescale(scale));
    entry.track.writeDatagram({ sequence: dg.sequence, timestamp, payload: dg.payload });
  }
  /**
   * Opens a PROBE bidi stream to receive bandwidth estimates from the publisher.
   * Returns immediately if recv bandwidth is not supported.
   *
   * Probe is best-effort telemetry: a stream-level failure (peer reset, FIN,
   * missing peer support, transport hiccup) is caught and logged, never
   * propagated to the connection. On exit the bandwidth/RTT signals are
   * cleared so consumers see them as stale.
   *
   * @internal
   */
  // Await the peer's advertised probe level, blocking until its SETUP arrives. The peer
  // MUST send exactly one SETUP, so this resolves once that stream is read.
  async #peerProbeLevel(peerSetup) {
    let setup = peerSetup.peek();
    while (setup === void 0) {
      setup = await peerSetup.changed();
    }
    return setup.probe;
  }
  async runProbe() {
    if (!this.#probe) return;
    if (this.version === Version2.DRAFT_01 || this.version === Version2.DRAFT_02) return;
    if (this.#peerSetup) {
      const probe = await this.#peerProbeLevel(this.#peerSetup);
      if (probe < ProbeLevel.Report) return;
    }
    try {
      const stream = await Stream.open(this.#quic);
      await stream.writer.u53(StreamId.Probe);
      for (; ; ) {
        const probe = await Probe.decodeMaybe(stream.reader, this.version);
        if (!probe) break;
        const prev = this.#probe.peek();
        const rtt = probe.rtt !== void 0 ? Milli(probe.rtt) : void 0;
        this.#probe.set({
          // `undefined` is the peer reporting "unknown", not an estimate of
          // zero; letting it through would become a real 0 bps ABR target.
          estimatedRecvRate: probe.bitrate,
          rtt: hasProbeRtt(this.version) ? rtt : rtt ?? prev.rtt
        });
      }
    } catch (err) {
      if (!this.#closed.signal.aborted) {
        console.warn("probe stream error", err);
      }
    } finally {
      this.#probe.set({});
    }
  }
  close() {
    this.#closed.abort();
    for (const { track } of this.#subscribes.values()) {
      track.close();
    }
    this.#subscribes.clear();
  }
};
var ConsumeBroadcast2 = class _ConsumeBroadcast extends Consumer4 {
  #subscriber;
  #path;
  constructor(subscriber, path, state) {
    super(state);
    this.#subscriber = subscriber;
    this.#path = path;
  }
  // Preserve the subclass (and its wire-backed info/fetchGroup) when the consume cache shares
  // this broadcast across callers.
  clone() {
    return new _ConsumeBroadcast(this.#subscriber, this.#path, this.shareState());
  }
  resolveTrackInfo(name) {
    return this.#subscriber.resolveTrackInfo(this.#path, name);
  }
  fetchGroup(name, sequence, options) {
    return this.#subscriber.fetchGroup(this.#path, name, sequence, options);
  }
};

// vendor/moq/js/net/src/lite/connection.ts
var Connection2 = class {
  // The URL of the connection.
  url;
  // The version of the connection as a human-readable string.
  version;
  // The wire transport this session runs over.
  transport;
  /** Whether the relay supports broadcast discovery; see {@link Established.discovery}. */
  discovery;
  // The version used for encoding/decoding.
  #version;
  // The established WebTransport session.
  #quic;
  // Use to receive/send session messages.
  #session;
  // Module for contributing tracks.
  #publisher;
  // Module for distributing tracks.
  #subscriber;
  /** The peer's PROBE estimates; see {@link Established.probe}. */
  probe;
  /** Random per-connection origin id. Shared by Publisher (for outbound hop
   * chains) and Subscriber (available for optional self-filtering on announces). */
  origin;
  // The peer's SETUP, recorded once its Setup stream is read (lite-05+). Streams whose
  // encoding depends on a negotiated capability (e.g. PROBE) wait on this. undefined
  // until the peer's SETUP arrives; stays undefined forever on older drafts.
  #peerSetup = new Signal(void 0);
  // Mirrors the role out of #peerSetup, so the public surface exposes the peer's declared
  // direction without handing out the whole SETUP (whose probe level gates our own streams).
  #peerRole = new Signal(void 0);
  // Written by the Subscriber as PROBE messages arrive.
  #probe = new Signal({});
  /**
   * The {@link Role} the peer advertised in its SETUP, for a server deciding whether the
   * peer's authorization grants the direction it intends to use.
   *
   * `undefined` until the peer's SETUP arrives, and forever on pre-lite-05 versions, which
   * carry no in-band role. {@link Role.Both} is the absence of the parameter, so it is what
   * a peer reports when it declines to declare a direction, sends a value we don't
   * recognize, or is a server (which never sends one).
   */
  get peerRole() {
    return this.#peerRole;
  }
  /**
   * Creates a new Connection instance.
   *
   * @internal
   */
  constructor({ url, quic, version: version2, session, discovery = true }) {
    this.url = url;
    this.#quic = quic;
    this.#session = session;
    this.version = versionName2(version2);
    this.#version = version2;
    this.transport = transportOf(quic);
    this.discovery = discovery;
    this.probe = this.#probe;
    this.origin = randomOrigin();
    this.#publisher = new Publisher2(this.#quic, this.#version, this.origin);
    this.#subscriber = new Subscriber3(this.#quic, this.#version, this.origin, this.#probe, this.#peerSetup);
    void this.#run();
  }
  /**
   * Closes the connection.
   */
  close() {
    this.#publisher.close();
    this.#subscriber.close();
    try {
      this.#quic.close();
    } catch {
    }
  }
  async #run() {
    const tasks = [this.#runSession(), this.#runBidis(), this.#runUnis()];
    if (hasSetupStream(this.#version)) {
      tasks.push(this.#sendSetup());
    }
    tasks.push(this.#subscriber.runProbe());
    if (hasDatagrams(this.#version)) {
      tasks.push(this.#subscriber.runDatagrams());
    }
    try {
      await Promise.all(tasks);
    } catch (err) {
      console.error("fatal error running connection", err);
    } finally {
      this.close();
    }
  }
  publish(path, producer) {
    this.#publisher.publish(path, producer);
  }
  announced(prefix = empty()) {
    return this.#subscriber.announced(prefix);
  }
  consume(path) {
    return this.#subscriber.consume(path);
  }
  /**
   * Watches a broadcast, live only while it is announced.
   *
   * @param path - The path of the broadcast to watch
   * @returns A reactive handle to the broadcast
   */
  announcedBroadcast(path) {
    return new Broadcast({ connection: this, path });
  }
  async #runSession() {
    if (!this.#session) {
      return;
    }
    try {
      for (; ; ) {
        const msg = await SessionInfo.decodeMaybe(this.#session.reader, this.#version);
        if (!msg) break;
      }
    } finally {
      console.debug("session stream closed");
    }
  }
  // Open the unidirectional Setup Stream, send our single SETUP, and FIN (lite-05+).
  // The browser uses WebTransport, which carries the request URI, so we advertise no
  // path and leave routing to the URL. The probe level reflects what this transport
  // can actually measure; we never pad, so we never advertise Increase.
  // Role stays Both: publish/consume are called after this point, so there is nothing
  // to narrow yet. The origin declares our session identity so the peer can filter
  // reflected announcements (lite-06 removed ANNOUNCE_REQUEST's exclude_hop for it).
  async #sendSetup() {
    const writer = await Writer.open(this.#quic);
    try {
      await writer.u53(DataType.Setup);
      const probe = await probeLevel(this.#quic, this.#version);
      await new Setup2({ probe, origin: this.origin }).encode(writer, this.#version);
      writer.close();
    } catch (err) {
      writer.reset(err);
      throw err;
    }
  }
  async #runBidis() {
    for (; ; ) {
      const stream = await Stream.accept(this.#quic);
      if (!stream) break;
      this.#runBidi(stream).catch((err) => {
        stream.writer.reset(err);
      }).finally(() => {
        stream.writer.close();
      });
    }
  }
  async #runBidi(stream) {
    const typ = await stream.reader.u53();
    if (typ === StreamId.Session) {
      throw new Error("duplicate session stream");
    } else if (typ === StreamId.Announce) {
      const msg = await AnnounceRequest.decode(stream.reader, this.#version);
      await this.#publisher.runAnnounce(msg, stream);
    } else if (typ === StreamId.Subscribe) {
      const msg = await Subscribe2.decode(stream.reader, this.#version);
      await this.#publisher.runSubscribe(msg, stream);
    } else if (typ === StreamId.Fetch) {
      const msg = await Fetch.decode(stream.reader, this.#version);
      await this.#publisher.runFetch(msg, stream);
    } else if (typ === StreamId.Track) {
      const msg = await Track.decode(stream.reader, this.#version);
      await this.#publisher.runTrackInfo(msg, stream);
    } else if (typ === StreamId.Probe) {
      await this.#publisher.runProbe(stream);
    } else if (typ === StreamId.Goaway) {
      const msg = await Goaway.decode(stream.reader, this.#version);
      console.info("received goaway:", msg.uri);
    } else {
      throw new Error(`unknown stream type: ${typ.toString()}`);
    }
  }
  async #runUnis() {
    const readers = new Readers(this.#quic);
    for (; ; ) {
      const stream = await readers.next();
      if (!stream) break;
      this.#runUni(stream).then(() => {
        stream.stop(new Error("cancel"));
      }).catch((err) => {
        stream.stop(err);
      });
    }
  }
  async #runUni(stream) {
    const typ = await stream.u53();
    if (typ === DataType.Group) {
      const msg = await Group2.decode(stream);
      await this.#subscriber.runGroup(msg, stream);
    } else if (typ === DataType.Setup) {
      const setup = await Setup2.decode(stream, this.#version);
      this.#peerSetup.set(setup);
      this.#peerRole.set(setup.role);
    } else {
      throw new Error(`unknown stream type: ${typ.toString()}`);
    }
  }
  /** Snapshot the transport's counters; see {@link Established.stats}. */
  async stats() {
    return transportStats(this.#quic);
  }
  get closed() {
    return this.#quic.closed.then(() => void 0);
  }
};
async function probeLevel(quic, version2) {
  const getStats = quic.getStats;
  if (typeof getStats !== "function") return ProbeLevel.None;
  let stats;
  try {
    stats = await getStats.call(quic);
  } catch {
    return ProbeLevel.None;
  }
  const rtt = hasProbeRtt(version2) ? stats.smoothedRtt : void 0;
  return stats.estimatedSendRate != null || rtt != null ? ProbeLevel.Report : ProbeLevel.None;
}

// vendor/moq/js/net/src/connection/handshake.ts
async function exchangeSetup(transport, version2, implementation, authorization) {
  const encoder = new TextEncoder();
  const params = new SetupOptions();
  params.setBytes(SetupOption.Implementation, encoder.encode(implementation));
  if (authorization !== void 0) setAuthorization(params, authorization);
  solicitIntoSetup(params);
  const self = randomOrigin();
  cluster_exports.intoSetup(params, self, version2);
  const setupMsg = new Setup({ parameters: params });
  const [writer, received] = await Promise.all([
    sendSetup(transport, version2, setupMsg),
    receiveSetup(transport, version2)
  ]);
  return {
    control: new Stream({ writer, reader: received.reader }),
    solicit: received.solicit,
    cluster: { self, peer: received.cluster }
  };
}
async function sendSetup(transport, version2, setupMsg) {
  const writer = await Writer.open(transport, { version: version2 });
  await writer.u53(Setup.id);
  await setupMsg.encode(writer, version2);
  return writer;
}
async function receiveSetup(transport, version2) {
  const uniReader = transport.incomingUnidirectionalStreams.getReader();
  const next = await uniReader.read();
  uniReader.releaseLock();
  if (next.done) throw new Error("no incoming uni stream for SETUP");
  const reader = new Reader(next.value, void 0, version2);
  const streamType = await reader.u53();
  if (streamType !== Setup.id) {
    throw new Error(`unexpected stream type on setup uni: 0x${streamType.toString(16)}`);
  }
  const setup = await Setup.decode(reader, version2);
  return {
    reader,
    solicit: solicitFromSetup(setup.parameters),
    cluster: cluster_exports.fromSetup(setup.parameters, version2)
  };
}

// vendor/moq/js/net/src/connection/accept.ts
async function accept(transport, url, props) {
  const protocol = transport.protocol;
  const discovery = props?.discovery ?? true;
  if (protocol === ALPN.DRAFT_21) {
    return acceptAlpn(transport, url, Version.DRAFT_21, discovery);
  }
  if (protocol === ALPN.DRAFT_20) {
    return acceptAlpn(transport, url, Version.DRAFT_20, discovery);
  }
  if (protocol === ALPN.DRAFT_19) {
    return acceptAlpn(transport, url, Version.DRAFT_19, discovery);
  } else if (protocol === ALPN.DRAFT_18) {
    return acceptAlpn(transport, url, Version.DRAFT_18, discovery);
  } else if (protocol === ALPN.DRAFT_17) {
    return acceptAlpn(transport, url, Version.DRAFT_17, discovery);
  } else if (protocol === ALPN.DRAFT_16) {
    return acceptSetup(transport, url, Version.DRAFT_16, discovery);
  } else if (protocol === ALPN.DRAFT_15) {
    return acceptSetup(transport, url, Version.DRAFT_15, discovery);
  } else if (protocol === ALPN_06_WIP) {
    return new Connection2({ url, quic: transport, version: Version2.DRAFT_06, discovery });
  } else if (protocol === ALPN_05) {
    return new Connection2({ url, quic: transport, version: Version2.DRAFT_05, discovery });
  } else if (protocol === ALPN_04) {
    return new Connection2({ url, quic: transport, version: Version2.DRAFT_04, discovery });
  } else if (protocol === ALPN_03) {
    return new Connection2({ url, quic: transport, version: Version2.DRAFT_03, discovery });
  } else if (protocol === ALPN2 || protocol === "" || protocol === void 0) {
    return acceptNegotiated(transport, url, props);
  } else {
    throw new Error(`unsupported WebTransport protocol: ${protocol}`);
  }
}
async function acceptAlpn(transport, url, version2, discovery) {
  const { control, solicit, cluster } = await exchangeSetup(transport, version2, "moq-lite-js");
  return new Connection({
    discovery,
    client: false,
    url,
    quic: transport,
    control,
    solicit,
    cluster,
    // v17+ uses NativeSession which manages its own request IDs; maxRequestId is unused.
    maxRequestId: 0n,
    version: version2
  });
}
async function acceptSetup(transport, url, version2, discovery) {
  const stream = await Stream.accept(transport);
  if (!stream) throw new Error("no incoming bidi stream for SETUP");
  const clientCompat = await stream.reader.u53();
  if (clientCompat !== StreamId.ClientCompat) {
    throw new Error(`unexpected client message type: 0x${clientCompat.toString(16)}`);
  }
  const client = await ClientSetup.decode(stream.reader, version2);
  await stream.writer.u53(StreamId.ServerCompat);
  const encoder = new TextEncoder();
  const params = new SetupOptions();
  params.setVarint(SetupOption.MaxRequestId, 42069n);
  params.setBytes(SetupOption.Implementation, encoder.encode("moq-lite-js"));
  solicitIntoSetup(params);
  const server = new ServerSetup({ version: version2, parameters: params });
  await server.encode(stream.writer, version2);
  const maxRequestId = 42069n;
  return new Connection({
    discovery,
    client: false,
    url,
    quic: transport,
    control: stream,
    maxRequestId,
    version: version2,
    solicit: solicitFromSetup(client.parameters)
  });
}
async function acceptNegotiated(transport, url, props) {
  const discovery = props?.discovery ?? true;
  const setupVersion = Version.DRAFT_14;
  const stream = await Stream.accept(transport);
  if (!stream) throw new Error("no incoming bidi stream for SETUP");
  const clientCompat = await stream.reader.u53();
  if (clientCompat !== StreamId.ClientCompat) {
    throw new Error(`unexpected client message type: 0x${clientCompat.toString(16)}`);
  }
  const client = await ClientSetup.decode(stream.reader, setupVersion);
  const allVersions = [...Object.values(Version2), ...Object.values(Version)];
  let selectedVersion;
  if (props?.version !== void 0) {
    selectedVersion = props.version;
  } else {
    const match = client.versions.find((v) => allVersions.includes(v));
    if (match === void 0) {
      throw new Error(
        `no common version found; client offered: ${client.versions.map((v) => v.toString(16)).join(", ")}`
      );
    }
    selectedVersion = match;
  }
  await stream.writer.u53(StreamId.ServerCompat);
  const encoder = new TextEncoder();
  const params = new SetupOptions();
  params.setVarint(SetupOption.MaxRequestId, 42069n);
  params.setBytes(SetupOption.Implementation, encoder.encode("moq-lite-js"));
  solicitIntoSetup(params);
  const server = new ServerSetup({ version: selectedVersion, parameters: params });
  await server.encode(stream.writer, setupVersion);
  if (Object.values(Version2).includes(selectedVersion)) {
    return new Connection2({
      url,
      quic: transport,
      version: selectedVersion,
      session: stream,
      discovery
    });
  } else if (Object.values(Version).includes(selectedVersion)) {
    const maxRequestId = client.parameters.getVarint(SetupOption.MaxRequestId) ?? 0n;
    return new Connection({
      discovery,
      client: false,
      url,
      quic: transport,
      control: stream,
      maxRequestId,
      version: selectedVersion,
      solicit: solicitFromSetup(client.parameters)
    });
  } else {
    throw new Error(`unsupported version: ${selectedVersion.toString(16)}`);
  }
}

// vendor/moq/js/net/src/connection/browser.ts
var import_bowser = __toESM(require_es5(), 1);
function isWebTransportUserAgentSupported(userAgent) {
  const browser = import_bowser.default.getParser(userAgent);
  if (browser.getEngine().name === "WebKit" || browser.getOS().name === "iOS") return false;
  const supported2 = browser.satisfies({
    // Fixed with 153.0.0, Firefox only allows two concurrent remote-initiated streams:
    // https://bugzilla.mozilla.org/show_bug.cgi?id=2046262
    firefox: ">=153.0"
  });
  if (supported2 === void 0) {
    return true;
  }
  return supported2;
}
function isWebTransportSupported() {
  if (typeof globalThis.WebTransport === "undefined") return false;
  if (typeof navigator === "undefined") return true;
  return isWebTransportUserAgentSupported(navigator.userAgent);
}

// vendor/moq/js/net/src/util/hex.ts
function toBytes(hex) {
  hex = hex.startsWith("0x") ? hex.slice(2) : hex;
  if (hex.length % 2) {
    throw new Error("invalid hex string length");
  }
  const matches = hex.match(/.{2}/g);
  if (!matches) {
    throw new Error("invalid hex string format");
  }
  return new Uint8Array(matches.map((byte) => parseInt(byte, 16)));
}

// vendor/moq/js/net/src/util/log.ts
function redact(url) {
  return `${url.origin}${url.pathname}`;
}
function dev() {
  const env = import.meta.env;
  if (typeof env === "undefined") return false;
  if (typeof env.DEV === "boolean") return env.DEV;
  return env.NODE_ENV !== "production" && env.MODE !== "production";
}

// vendor/moq/js/net/src/connection/connect.ts
var DEFAULT_WEBSOCKET_DELAY_MS = 500;
var NO_DISCOVERY_HOSTS = ["mediaoverquic.com"];
function defaultDiscovery(url) {
  return !NO_DISCOVERY_HOSTS.some((host) => url.hostname.endsWith(host));
}
var websocketWon = /* @__PURE__ */ new Set();
var NEVER_ABORTED = new AbortController().signal;
async function connect(url, props) {
  const signal = props?.signal ?? NEVER_ABORTED;
  signal.throwIfAborted();
  const authorization = props?.authorization === void 0 ? void 0 : copyAuthorization(props.authorization);
  if (authorization !== void 0 && props?.transport === void 0) {
    if (props?.websocket?.enabled !== false) {
      throw new Error("authorization requires WebSocket fallback to be disabled");
    }
    const protocols = props.webtransport?.protocols;
    if (protocols?.length !== 1 || protocols[0] !== ALPN.DRAFT_21) {
      throw new Error("authorization requires WebTransport protocol moqt-21 only");
    }
  }
  const { promise: abort, resolve: resolve2 } = Promise.withResolvers();
  const onAbort = () => resolve2();
  signal.addEventListener("abort", onAbort, { once: true });
  const pending = connectInner(url, props, abort, authorization);
  try {
    const connection = await Promise.race([pending, abort.then(() => void 0)]);
    if (connection && !signal.aborted) return connection;
    pending.then((conn) => conn.close()).catch(() => {
    });
    throw signal.reason;
  } finally {
    signal.removeEventListener("abort", onAbort);
  }
}
async function connectInner(url, props, abort, authorization) {
  const discovery = props?.discovery ?? defaultDiscovery(url);
  if (props?.transport) {
    const transport = props.transport;
    void abort.then(() => transport.close());
    return connectTransport(url, transport, discovery, authorization);
  }
  const { promise: raced, resolve: done } = Promise.withResolvers();
  const cancel = Promise.race([raced, abort]);
  const webtransport = isWebTransportSupported() ? connectWebTransport(url, cancel, props?.webtransport) : void 0;
  const headstart = !webtransport || websocketWon.has(url.toString()) ? 0 : props?.websocket?.delay ?? DEFAULT_WEBSOCKET_DELAY_MS;
  const websocket = props?.websocket?.enabled !== false ? connectWebSocket(props?.websocket?.url ?? url, headstart, cancel) : void 0;
  if (!websocket && !webtransport) {
    throw new Error("no transport available; WebTransport not supported and WebSocket is disabled");
  }
  const session = await Promise.any(
    webtransport !== void 0 ? websocket !== void 0 ? [websocket, webtransport] : [webtransport] : [websocket]
  );
  done();
  if (!session) throw new Error("no transport available");
  void abort.then(() => session.close());
  if (session instanceof qmux_default) {
    if (dev()) console.warn(redact(url), "connected via WebSocket");
    websocketWon.add(url.toString());
  } else if (dev()) {
    console.debug(redact(url), "connected via WebTransport");
  }
  return await connectTransport(url, session, discovery, authorization);
}
async function connectTransport(url, session, discovery, authorization) {
  try {
    return await establishTransport(url, session, discovery, authorization);
  } catch (err) {
    session.close();
    throw err;
  }
}
async function establishTransport(url, session, discovery, authorization) {
  const protocol = session.protocol || void 0;
  if (dev()) console.debug(redact(url), "negotiated ALPN:", protocol ?? "(none)");
  if (authorization !== void 0 && protocol !== ALPN.DRAFT_21) {
    throw new Error(`authorization requires negotiated protocol ${ALPN.DRAFT_21}`);
  }
  let setupVersion;
  const modernVersion = protocol === ALPN.DRAFT_21 ? Version.DRAFT_21 : protocol === ALPN.DRAFT_20 ? Version.DRAFT_20 : protocol === ALPN.DRAFT_19 ? Version.DRAFT_19 : protocol === ALPN.DRAFT_18 ? Version.DRAFT_18 : protocol === ALPN.DRAFT_17 ? Version.DRAFT_17 : void 0;
  if (modernVersion !== void 0) {
    return await handshakeAlpn(url, session, modernVersion, discovery, authorization);
  } else if (protocol === ALPN.DRAFT_16) {
    setupVersion = Version.DRAFT_16;
  } else if (protocol === ALPN.DRAFT_15) {
    setupVersion = Version.DRAFT_15;
  } else if (protocol === ALPN_06_WIP) {
    return new Connection2({ url, quic: session, version: Version2.DRAFT_06, discovery });
  } else if (protocol === ALPN_05) {
    return new Connection2({ url, quic: session, version: Version2.DRAFT_05, discovery });
  } else if (protocol === ALPN_04) {
    return new Connection2({ url, quic: session, version: Version2.DRAFT_04, discovery });
  } else if (protocol === ALPN_03) {
    return new Connection2({ url, quic: session, version: Version2.DRAFT_03, discovery });
  } else if (protocol === ALPN2 || protocol === "" || protocol === void 0) {
    setupVersion = Version.DRAFT_14;
  } else {
    throw new Error(`unsupported WebTransport protocol: ${protocol}`);
  }
  const stream = await Stream.open(session);
  await stream.writer.u53(StreamId.ClientCompat);
  const encoder = new TextEncoder();
  const params = new SetupOptions();
  params.setVarint(SetupOption.MaxRequestId, 42069n);
  params.setBytes(SetupOption.Implementation, encoder.encode("moq-lite-js"));
  solicitIntoSetup(params);
  const client = new ClientSetup({
    versions: setupVersion === Version.DRAFT_16 ? [Version.DRAFT_16] : setupVersion === Version.DRAFT_15 ? [Version.DRAFT_15] : [Version2.DRAFT_02, Version2.DRAFT_01, Version.DRAFT_14],
    parameters: params
  });
  await client.encode(stream.writer, setupVersion);
  const serverCompat = await stream.reader.u53();
  if (serverCompat !== StreamId.ServerCompat) {
    throw new Error(`unsupported server message type: ${serverCompat.toString()}`);
  }
  const server = await ServerSetup.decode(stream.reader, setupVersion);
  if (Object.values(Version2).includes(server.version)) {
    return new Connection2({
      url,
      quic: session,
      version: server.version,
      session: stream,
      discovery
    });
  } else if (Object.values(Version).includes(server.version)) {
    const maxRequestId = server.parameters.getVarint(SetupOption.MaxRequestId) ?? 0n;
    return new Connection({
      discovery,
      client: true,
      url,
      quic: session,
      control: stream,
      maxRequestId,
      version: server.version,
      solicit: solicitFromSetup(server.parameters)
    });
  } else {
    throw new Error(`unsupported server version: ${server.version.toString()}`);
  }
}
async function handshakeAlpn(url, session, version2, discovery, authorization) {
  const { control, solicit, cluster } = await exchangeSetup(session, version2, "moq-lite-js", authorization);
  return new Connection({
    discovery,
    client: true,
    url,
    quic: session,
    control,
    solicit,
    cluster,
    // v17+ uses NativeSession which manages its own request IDs; maxRequestId is unused.
    maxRequestId: 0n,
    version: version2
  });
}
function pemToDer(pem) {
  const match = pem.match(/-----BEGIN CERTIFICATE-----([\s\S]+?)-----END CERTIFICATE-----/);
  if (!match) {
    throw new Error("invalid PEM certificate: missing -----BEGIN/END CERTIFICATE----- armor");
  }
  const binary = atob(match[1].replace(/\s+/g, ""));
  const der = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    der[i] = binary.charCodeAt(i);
  }
  return der;
}
async function certificateHash(cert) {
  const der = typeof cert === "string" ? pemToDer(cert) : cert;
  const digest = await crypto.subtle.digest("SHA-256", der);
  return new Uint8Array(digest);
}
async function resolveCertificateHashes(options) {
  const hashes = [];
  for (const hash of options?.serverCertificateHashes ?? []) {
    const value = typeof hash.value === "string" ? toBytes(hash.value) : hash.value;
    hashes.push({ algorithm: hash.algorithm ?? "sha-256", value });
  }
  if (options?.serverCertificate !== void 0) {
    hashes.push({ algorithm: "sha-256", value: await certificateHash(options.serverCertificate) });
  }
  return hashes.length > 0 ? hashes : void 0;
}
async function connectWebTransport(url, cancel, options) {
  let finalUrl = url;
  const { serverCertificate: _cert, serverCertificateHashes: _hashes, ...webtransport } = options ?? {};
  const finalOptions = {
    allowPooling: false,
    congestionControl: "low-latency",
    protocols: [
      ALPN_05,
      ALPN_04,
      ALPN_03,
      ALPN2,
      ALPN.DRAFT_21,
      ALPN.DRAFT_20,
      ALPN.DRAFT_19,
      ALPN.DRAFT_18,
      ALPN.DRAFT_17,
      ALPN.DRAFT_16,
      ALPN.DRAFT_15
    ],
    ...webtransport
  };
  const hashes = await resolveCertificateHashes(options) ?? [];
  if (url.protocol === "http:") {
    const fingerprintUrl = new URL(url);
    fingerprintUrl.pathname = "/certificate.sha256";
    fingerprintUrl.search = "";
    if (dev()) {
      console.debug(
        redact(fingerprintUrl),
        "performing an insecure fingerprint fetch; use https:// in production"
      );
    }
    const fingerprint = await Promise.race([fetch(fingerprintUrl), cancel]);
    if (!fingerprint) return void 0;
    const fingerprintText = await Promise.race([fingerprint.text(), cancel]);
    if (fingerprintText === void 0) return void 0;
    hashes.push({ algorithm: "sha-256", value: toBytes(fingerprintText) });
    finalUrl = new URL(url);
    finalUrl.protocol = "https:";
  }
  if (hashes.length > 0) {
    finalOptions.serverCertificateHashes = hashes;
  }
  const quic = new WebTransport(finalUrl, finalOptions);
  quic.closed.catch(() => {
  });
  const loaded = await Promise.race([quic.ready.then(() => true), cancel]);
  if (!loaded) {
    quic.close();
    return void 0;
  }
  return quic;
}
async function connectWebSocket(url, delay, cancel) {
  const timer = new Promise((resolve2) => setTimeout(resolve2, delay));
  const active = await Promise.race([cancel, timer.then(() => true)]);
  if (!active) return void 0;
  const versions = {
    // Lite.ALPN_06_WIP omitted on purpose: lite-06 is work-in-progress, not advertised by default.
    [ALPN_05]: null,
    [ALPN_04]: null,
    [ALPN_03]: null,
    [ALPN2]: null,
    [ALPN.DRAFT_21]: "qmux-01",
    [ALPN.DRAFT_20]: "qmux-01",
    [ALPN.DRAFT_19]: "qmux-01",
    [ALPN.DRAFT_18]: "qmux-01",
    [ALPN.DRAFT_17]: null,
    [ALPN.DRAFT_16]: null,
    [ALPN.DRAFT_15]: null
  };
  const quic = new qmux_default(url, {
    protocols: Object.keys(versions),
    versions
  });
  const loaded = await Promise.race([quic.ready.then(() => true), cancel]);
  if (!loaded) {
    quic.close();
    return void 0;
  }
  return quic;
}

// vendor/moq/js/net/src/connection/reload.ts
var DEFAULT_TIMEOUT = 1e4;
var Reload = class {
  /** Relay URL to connect to; updating it triggers a reconnect. */
  url;
  /** Whether reconnecting is active. */
  enabled;
  /** Current connection status. */
  status = new Signal("disconnected");
  /** The currently established session, or undefined while disconnected. */
  established = new Signal(void 0);
  /**
   * The current connection's PROBE estimates, spanning reconnects.
   *
   * Undefined while disconnected: the estimates belong to a single connection.
   * See {@link Established.probe}.
   */
  probe;
  /** WebTransport options applied to each connection attempt (not reactive). */
  webtransport;
  /** WebSocket fallback options applied to each connection attempt (not reactive). */
  websocket;
  /**
   * Whether the relay supports broadcast discovery, applied to each connection attempt (not
   * reactive). Undefined defers to the default for the URL. See {@link Established.discovery}.
   */
  discovery;
  /** Backoff settings for the reconnect loop. */
  delay;
  /** The reactive effect scope driving the connect loop; closed by {@link Reload.close}. */
  #signals = new Effect();
  /**
   * Resolves when the reconnect loop stops via {@link Reload.close}.
   *
   * Rejects when the loop gives up instead, carrying the failure that was in flight when the
   * retry window expired.
   */
  closed;
  #closedResolve;
  #closedReject;
  // The current wait between attempts, doubling per failure, and when the retry window expires.
  // Both are undefined between sequences, so a later edit to `delay` applies to the next one.
  #delay;
  #deadline;
  // Increased by 1 each time to trigger a reload.
  #tick = new Signal(0);
  // True after the browser freezes or hides the page until it visibly resumes.
  #suspended = new Signal(false);
  // Use the serialized URL as the reactive connection key. URL objects use identity
  // equality, but replacing one with an equivalent instance should not reconnect.
  #url;
  constructor(props) {
    this.url = Signal.from(props?.url);
    this.enabled = Signal.from(props?.enabled ?? false);
    this.delay = props?.delay ?? { initial: 1e3, multiplier: 2, max: 5e3 };
    this.webtransport = props?.webtransport;
    this.websocket = props?.websocket;
    this.discovery = props?.discovery;
    this.closed = new Promise((resolve2, reject) => {
      this.#closedResolve = resolve2;
      this.#closedReject = reject;
    });
    this.closed.catch(() => {
    });
    if (typeof window !== "undefined" && typeof document !== "undefined") {
      this.#signals.event(window, "pagehide", () => this.#suspended.set(true));
      this.#signals.event(window, "pageshow", () => this.#suspended.set(false));
      this.#signals.event(window, "unload", () => this.#suspended.set(true));
      this.#signals.event(document, "visibilitychange", () => {
        if (!document.hidden) this.#suspended.set(false);
      });
    }
    this.probe = this.#signals.computed((effect) => {
      const connection = effect.get(this.established);
      return connection && effect.get(connection.probe);
    });
    this.#url = this.#signals.computed((effect) => effect.get(this.url)?.href);
    this.#signals.run(this.#connect.bind(this));
  }
  #connect(effect) {
    effect.get(this.#tick);
    const suspended = effect.get(this.#suspended);
    const enabled = effect.get(this.enabled);
    if (!enabled || suspended) return;
    const href = effect.get(this.#url);
    if (!href) return;
    const url = new URL(href);
    effect.set(this.status, "connecting", "disconnected");
    const signal = effect.abort;
    effect.spawn(async () => {
      let connected;
      try {
        const connection = await connect(url, {
          websocket: this.websocket,
          webtransport: this.webtransport,
          discovery: this.discovery,
          signal
        });
        effect.cleanup(() => connection.close());
        if (signal.aborted) return;
        effect.set(this.established, connection);
        effect.set(this.status, "connected", "disconnected");
        connected = performance.now();
        const closed = await Promise.race([effect.cancel, connection.closed.then(() => true)]);
        if (!closed) return;
        console.warn("connection closed, reconnecting");
        this.#retry(effect, connected);
      } catch (err) {
        if (signal.aborted) return;
        console.warn("connection error:", err);
        this.#retry(effect, connected, err);
      }
    });
  }
  /**
   * Schedule the next connect attempt after the current backoff, or stop once the retry window
   * has expired. `connected` is when the dead session was established, if it ever was, and
   * `cause` the error that killed it, if it died with one.
   */
  #retry(effect, connected, cause) {
    this.established.set(void 0);
    this.status.set("disconnected");
    if (connected !== void 0 && performance.now() - connected >= this.delay.initial) {
      this.#delay = void 0;
      this.#deadline = void 0;
    }
    const now = performance.now();
    const timeout = this.delay.timeout ?? DEFAULT_TIMEOUT;
    this.#delay ??= this.delay.initial;
    this.#deadline ??= timeout > 0 ? now + timeout : Number.POSITIVE_INFINITY;
    if (now >= this.#deadline) {
      console.warn("reconnect timed out");
      this.#closedReject(cause === void 0 ? new Error("reconnect timed out") : error(cause));
      return;
    }
    const wait = Math.min(this.#delay * (0.5 + Math.random() / 2), this.#deadline - now);
    this.#delay = Math.min(this.#delay * this.delay.multiplier, this.delay.max);
    const tick = this.#tick.peek() + 1;
    effect.timer(() => this.#tick.update((prev) => Math.max(prev, tick)), wait);
  }
  /**
   * Subscribe to broadcast announcements under an optional prefix, spanning reconnects.
   *
   * The same {@link Announce.Consumer} stream as {@link Established.announced}, but everything active
   * is retracted (an `active: false` update) whenever the connection drops and re-announced on
   * reconnect, so a consumer draining `next()` never clings to a dead route across a reconnect.
   *
   * Stays empty while the relay lacks {@link Established.discovery}.
   */
  announced(prefix = empty()) {
    const producer = new Producer(prefix);
    const consumer = producer.consume();
    const pump = new Effect();
    pump.run((effect) => {
      const conn = effect.get(this.established);
      if (!conn) return;
      if (!conn.discovery) return;
      const upstream = conn.announced(prefix);
      effect.cleanup(() => upstream.close());
      const active = /* @__PURE__ */ new Set();
      effect.spawn(async () => {
        try {
          for (; ; ) {
            const entry = await Promise.race([effect.cancel, upstream.next()]);
            if (!entry) break;
            if (entry.active) active.add(entry.path);
            else active.delete(entry.path);
            producer.append(entry);
          }
        } catch {
        } finally {
          if (consumer.closed.peek() === void 0) {
            for (const path of active) {
              producer.append({ path, active: false });
            }
          }
        }
      });
    });
    this.#signals.cleanup(() => pump.close());
    void consumer.closed.then(() => pump.close());
    return consumer;
  }
  /**
   * A reactive handle to one broadcast, spanning reconnects.
   *
   * The same {@link Announce.Broadcast} as {@link Established.announcedBroadcast}, but it
   * follows the reconnect loop: the broadcast drops to `undefined` when the connection dies
   * and resolves again once the new connection announces the path. Use it instead of
   * consuming off {@link Reload.established} whenever the broadcast may come online after you
   * do, which is exactly the case a blind `consume` loses.
   *
   * Close the handle when done; {@link Reload.close} only drops it to `undefined`.
   */
  announcedBroadcast(path) {
    return new Broadcast({ connection: this.established, path });
  }
  /**
   * Snapshot the live connection's transport counters, or undefined while disconnected.
   * See {@link Established.stats}.
   */
  async stats() {
    return this.established.peek()?.stats();
  }
  /** Stop reconnecting, close the current connection, and resolve {@link Reload.closed}. */
  close() {
    this.#signals.close();
    this.#closedResolve();
  }
};
export {
  announced_exports as Announce,
  broadcast_exports as Broadcast,
  connection_exports as Connection,
  group_exports as Group,
  path_exports as Path,
  RemoteError,
  src_exports as Signals,
  time_exports as Time,
  track_exports as Track,
  varint_exports as Varint
};

/*!
BUNDLED LICENSES FOR media-moq-library.mjs

@moq/net source at 3aedaf3503146c664479f6fc53a50b36bb5e816d
@moq/signals source at 3aedaf3503146c664479f6fc53a50b36bb5e816d
@moq/qmux@0.3.3
@moq/web-socket-stream@0.1.1
------------------------------------------------------------------------
MIT License

Copyright (c) 2023 Luke Curley

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
Apache License
                           Version 2.0, January 2004
                        http://www.apache.org/licenses/

   TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION

   1. Definitions.

      "License" shall mean the terms and conditions for use, reproduction,
      and distribution as defined by Sections 1 through 9 of this document.

      "Licensor" shall mean the copyright owner or entity authorized by
      the copyright owner that is granting the License.

      "Legal Entity" shall mean the union of the acting entity and all
      other entities that control, are controlled by, or are under common
      control with that entity. For the purposes of this definition,
      "control" means (i) the power, direct or indirect, to cause the
      direction or management of such entity, whether by contract or
      otherwise, or (ii) ownership of fifty percent (50%) or more of the
      outstanding shares, or (iii) beneficial ownership of such entity.

      "You" (or "Your") shall mean an individual or Legal Entity
      exercising permissions granted by this License.

      "Source" form shall mean the preferred form for making modifications,
      including but not limited to software source code, documentation
      source, and configuration files.

      "Object" form shall mean any form resulting from mechanical
      transformation or translation of a Source form, including but
      not limited to compiled object code, generated documentation,
      and conversions to other media types.

      "Work" shall mean the work of authorship, whether in Source or
      Object form, made available under the License, as indicated by a
      copyright notice that is included in or attached to the work
      (an example is provided in the Appendix below).

      "Derivative Works" shall mean any work, whether in Source or Object
      form, that is based on (or derived from) the Work and for which the
      editorial revisions, annotations, elaborations, or other modifications
      represent, as a whole, an original work of authorship. For the purposes
      of this License, Derivative Works shall not include works that remain
      separable from, or merely link (or bind by name) to the interfaces of,
      the Work and Derivative Works thereof.

      "Contribution" shall mean any work of authorship, including
      the original version of the Work and any modifications or additions
      to that Work or Derivative Works thereof, that is intentionally
      submitted to Licensor for inclusion in the Work by the copyright owner
      or by an individual or Legal Entity authorized to submit on behalf of
      the copyright owner. For the purposes of this definition, "submitted"
      means any form of electronic, verbal, or written communication sent
      to the Licensor or its representatives, including but not limited to
      communication on electronic mailing lists, source code control systems,
      and issue tracking systems that are managed by, or on behalf of, the
      Licensor for the purpose of discussing and improving the Work, but
      excluding communication that is conspicuously marked or otherwise
      designated in writing by the copyright owner as "Not a Contribution."

      "Contributor" shall mean Licensor and any individual or Legal Entity
      on behalf of whom a Contribution has been received by Licensor and
      subsequently incorporated within the Work.

   2. Grant of Copyright License. Subject to the terms and conditions of
      this License, each Contributor hereby grants to You a perpetual,
      worldwide, non-exclusive, no-charge, royalty-free, irrevocable
      copyright license to reproduce, prepare Derivative Works of,
      publicly display, publicly perform, sublicense, and distribute the
      Work and such Derivative Works in Source or Object form.

   3. Grant of Patent License. Subject to the terms and conditions of
      this License, each Contributor hereby grants to You a perpetual,
      worldwide, non-exclusive, no-charge, royalty-free, irrevocable
      (except as stated in this section) patent license to make, have made,
      use, offer to sell, sell, import, and otherwise transfer the Work,
      where such license applies only to those patent claims licensable
      by such Contributor that are necessarily infringed by their
      Contribution(s) alone or by combination of their Contribution(s)
      with the Work to which such Contribution(s) was submitted. If You
      institute patent litigation against any entity (including a
      cross-claim or counterclaim in a lawsuit) alleging that the Work
      or a Contribution incorporated within the Work constitutes direct
      or contributory patent infringement, then any patent licenses
      granted to You under this License for that Work shall terminate
      as of the date such litigation is filed.

   4. Redistribution. You may reproduce and distribute copies of the
      Work or Derivative Works thereof in any medium, with or without
      modifications, and in Source or Object form, provided that You
      meet the following conditions:

      (a) You must give any other recipients of the Work or
          Derivative Works a copy of this License; and

      (b) You must cause any modified files to carry prominent notices
          stating that You changed the files; and

      (c) You must retain, in the Source form of any Derivative Works
          that You distribute, all copyright, patent, trademark, and
          attribution notices from the Source form of the Work,
          excluding those notices that do not pertain to any part of
          the Derivative Works; and

      (d) If the Work includes a "NOTICE" text file as part of its
          distribution, then any Derivative Works that You distribute must
          include a readable copy of the attribution notices contained
          within such NOTICE file, excluding those notices that do not
          pertain to any part of the Derivative Works, in at least one
          of the following places: within a NOTICE text file distributed
          as part of the Derivative Works; within the Source form or
          documentation, if provided along with the Derivative Works; or,
          within a display generated by the Derivative Works, if and
          wherever such third-party notices normally appear. The contents
          of the NOTICE file are for informational purposes only and
          do not modify the License. You may add Your own attribution
          notices within Derivative Works that You distribute, alongside
          or as an addendum to the NOTICE text from the Work, provided
          that such additional attribution notices cannot be construed
          as modifying the License.

      You may add Your own copyright statement to Your modifications and
      may provide additional or different license terms and conditions
      for use, reproduction, or distribution of Your modifications, or
      for any such Derivative Works as a whole, provided Your use,
      reproduction, and distribution of the Work otherwise complies with
      the conditions stated in this License.

   5. Submission of Contributions. Unless You explicitly state otherwise,
      any Contribution intentionally submitted for inclusion in the Work
      by You to the Licensor shall be under the terms and conditions of
      this License, without any additional terms or conditions.
      Notwithstanding the above, nothing herein shall supersede or modify
      the terms of any separate license agreement you may have executed
      with Licensor regarding such Contributions.

   6. Trademarks. This License does not grant permission to use the trade
      names, trademarks, service marks, or product names of the Licensor,
      except as required for reasonable and customary use in describing the
      origin of the Work and reproducing the content of the NOTICE file.

   7. Disclaimer of Warranty. Unless required by applicable law or
      agreed to in writing, Licensor provides the Work (and each
      Contributor provides its Contributions) on an "AS IS" BASIS,
      WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or
      implied, including, without limitation, any warranties or conditions
      of TITLE, NON-INFRINGEMENT, MERCHANTABILITY, or FITNESS FOR A
      PARTICULAR PURPOSE. You are solely responsible for determining the
      appropriateness of using or redistributing the Work and assume any
      risks associated with Your exercise of permissions under this License.

   8. Limitation of Liability. In no event and under no legal theory,
      whether in tort (including negligence), contract, or otherwise,
      unless required by applicable law (such as deliberate and grossly
      negligent acts) or agreed to in writing, shall any Contributor be
      liable to You for damages, including any direct, indirect, special,
      incidental, or consequential damages of any character arising as a
      result of this License or out of the use or inability to use the
      Work (including but not limited to damages for loss of goodwill,
      work stoppage, computer failure or malfunction, or any and all
      other commercial damages or losses), even if such Contributor
      has been advised of the possibility of such damages.

   9. Accepting Warranty or Additional Liability. While redistributing
      the Work or Derivative Works thereof, You may choose to offer,
      and charge a fee for, acceptance of support, warranty, indemnity,
      or other liability obligations and/or rights consistent with this
      License. However, in accepting such obligations, You may act only
      on Your own behalf and on Your sole responsibility, not on behalf
      of any other Contributor, and only if You agree to indemnify,
      defend, and hold each Contributor harmless for any liability
      incurred by, or claims asserted against, such Contributor by reason
      of your accepting any such warranty or additional liability.

   END OF TERMS AND CONDITIONS

   APPENDIX: How to apply the Apache License to your work.

      To apply the Apache License to your work, attach the following
      boilerplate notice, with the fields enclosed by brackets "[]"
      replaced with your own identifying information. (Don't include
      the brackets!)  The text should be enclosed in the appropriate
      comment syntax for the file format. We also recommend that a
      file or class name and description of purpose be included on the
      same "printed page" as the copyright notice for easier
      identification within third-party archives.

   Copyright [yyyy] [name of copyright owner]

   Licensed under the Apache License, Version 2.0 (the "License");
   you may not use this file except in compliance with the License.
   You may obtain a copy of the License at

       http://www.apache.org/licenses/LICENSE-2.0

   Unless required by applicable law or agreed to in writing, software
   distributed under the License is distributed on an "AS IS" BASIS,
   WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
   See the License for the specific language governing permissions and
   limitations under the License.

@zod/mini@4.5.0
zod@4.6.5
------------------------------------------------------------------------
MIT License

Copyright (c) 2025 Colin McDonnell

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

async-mutex@0.5.0
------------------------------------------------------------------------
The MIT License (MIT)

Copyright (c) 2016 Christian Speckner <cnspeckn@googlemail.com>

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.

bowser@2.14.1
------------------------------------------------------------------------
Copyright 2015, Dustin Diaz (the "Original Author")
All rights reserved.

MIT License

Permission is hereby granted, free of charge, to any person
obtaining a copy of this software and associated documentation
files (the "Software"), to deal in the Software without
restriction, including without limitation the rights to use,
copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the
Software is furnished to do so, subject to the following
conditions:

The above copyright notice and this permission notice shall be
included in all copies or substantial portions of the Software.

Distributions of all or part of the Software intended to be used
by the recipients as they would use the unmodified Software,
containing modifications that substantially alter, remove, or
disable functionality of the Software, outside of the documented
configuration mechanisms provided by the Software, shall be
modified such that the Original Author's bug reporting email
addresses and urls are either replaced with the contact information
of the parties responsible for the changes, or removed entirely.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES
OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT
HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,
WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR
OTHER DEALINGS IN THE SOFTWARE.


Except where noted, this license applies to any and all software
programs and associated documentation files created by the
Original Author, when distributed with the Software.

tslib@2.8.1
------------------------------------------------------------------------
Copyright (c) Microsoft Corporation.

Permission to use, copy, modify, and/or distribute this software for any
purpose with or without fee is hereby granted.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH
REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY
AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT,
INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES WHATSOEVER RESULTING FROM
LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR
OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR
PERFORMANCE OF THIS SOFTWARE.

*/
