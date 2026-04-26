// Type definitions mirroring the schema emitted by `bock-dump-vocab`.
// Kept in sync with crates/bock-vocab/src/schema.rs.

export interface Vocab {
  version: string;
  language: LanguageVocab;
  stdlib: StdlibVocab;
  diagnostics: DiagnosticsVocab;
  tooling: ToolingVocab;
}

export interface LanguageVocab {
  keywords: Keyword[];
  operators: Operator[];
  annotations: Annotation[];
  strictness_levels: StrictnessLevel[];
  primitive_types: PrimitiveType[];
  prelude_types: Symbol[];
  prelude_functions: Symbol[];
  prelude_traits: Symbol[];
  prelude_constructors: Symbol[];
}

export interface Keyword {
  name: string;
  category: string;
  spec_ref?: string;
}

export interface Operator {
  symbol: string;
  precedence?: number;
  associativity: string;
  kind: string;
  spec_ref?: string;
}

export interface Annotation {
  name: string;
  params: string;
  purpose: string;
  spec_ref?: string;
}

export interface StrictnessLevel {
  name: string;
  description: string;
  spec_ref?: string;
}

export interface PrimitiveType {
  name: string;
  spec_ref?: string;
}

export interface Symbol {
  name: string;
  kind: string;
  signature: string;
  doc?: string;
  spec_ref?: string;
  since?: string;
}

export interface StdlibVocab {
  modules: Module[];
  builtin_methods: BuiltinMethodGroup[];
  builtin_globals: string[];
}

export interface Module {
  path: string;
  types: Symbol[];
  functions: Symbol[];
  effects: Symbol[];
  traits: Symbol[];
  spec_ref?: string;
}

export interface BuiltinMethodGroup {
  receiver: string;
  methods: string[];
}

export interface SymbolVocab extends Symbol {
  module?: string;
}

export interface DiagnosticsVocab {
  codes: DiagnosticCode[];
}

export interface DiagnosticCode {
  code: string;
  severity: string;
  summary: string;
  description: string;
  bad_example?: string;
  good_example?: string;
  spec_refs: string[];
  related_codes: string[];
}

export interface ToolingVocab {
  targets: Target[];
  ai_providers: string[];
  commands: CommandInfo[];
}

export interface Target {
  id: string;
  display_name: string;
}

export interface CommandInfo {
  name: string;
  summary: string;
}
