# frozen_string_literal: true

# Differential checks for the extension fuzz targets. Only the native
# layer is loaded (Init_nosj, no lib/nosj.rb), so the error classes the
# extension looks up by name are defined here first.
module NOSJ
  class Error < StandardError; end
  class ParserError < Error; end
  class GeneratorError < Error; end
  class NestingError < Error; end
  class PatchError < Error; end
end

module NOSJFuzz
  # A reference-side rejection: the case must fail natively too.
  RefFail = Class.new(StandardError)

  PARSE_FAIL = [NOSJ::ParserError, NOSJ::NestingError].freeze
  EDIT_FAIL = (PARSE_FAIL + [NOSJ::PatchError, NOSJ::GeneratorError,
    KeyError, ArgumentError, TypeError]).freeze
  PRETTY = {indent: "  ", space: " ", object_nl: "\n", array_nl: "\n"}.freeze
  # A copy op can double the document (copy root into a child), so a
  # long op list can grow it exponentially; keep runs tractable.
  MAX_PATCH_OPS = 12

  module_function

  def utf8(bytes) = bytes.dup.force_encoding(Encoding::UTF_8)

  def try_parse(s, opts = nil)
    [:ok, NOSJ.parse_native(s, opts)]
  rescue *PARSE_FAIL
    [:err, nil]
  end

  # --- reformat: minify/reformat vs parse-then-generate ---------------

  def reformat_case(bytes)
    s = utf8(bytes)
    status, value = try_parse(s)

    unless NOSJ.valid_native(s, nil) == (status == :ok)
      raise "valid? disagrees with parse on acceptance"
    end
    stats_ok = begin
      NOSJ.stats_native(s, nil)
      true
    rescue *PARSE_FAIL
      false
    end
    raise "stats disagrees with parse on acceptance" unless stats_ok == (status == :ok)

    min_status, min = begin
      [:ok, NOSJ.reformat_native(s, nil)]
    rescue *PARSE_FAIL
      [:err, nil]
    rescue NOSJ::GeneratorError
      [:generator, nil]
    end

    if status == :err
      # The pipe may abort with GeneratorError (lone-surrogate key,
      # non-finite float) before the parser reaches whatever made the
      # whole document unparseable; any refusal is a refusal.
      raise "reformat accepted what parse refused" unless [:err, :generator].include?(min_status)
      return
    end
    if min_status == :generator
      # On a document parse accepts, only an overflow-to-Infinity
      # float (1e999, a 300-digit integer with a small exponent, maybe
      # hidden behind a duplicate key parse would discard) or a
      # lone-surrogate object key may abort the pipe. Rerunning with
      # allow_nan separates them: it lifts the float refusal but not
      # the key refusal, and the key requires a \u escape.
      begin
        NOSJ.reformat_native(s, {allow_nan: true})
      rescue NOSJ::GeneratorError
        raise "GeneratorError without a \\u escape in source" unless s.include?("\\u")
      end
      return
    end
    raise "reformat refused what parse accepted" unless min_status == :ok

    reparsed = stage("reparse minify output #{min.inspect[0, 200]}") do
      NOSJ.parse_native(min, nil)
    end
    raise "minify does not round-trip: #{min.inspect[0, 200]}" unless reparsed.eql?(value)
    raise "minify is not idempotent" unless stage("re-minify") {
      NOSJ.reformat_native(min, nil)
    } == min
    pretty = stage("pretty") { NOSJ.reformat_native(s, PRETTY) }
    raise "pretty does not round-trip" unless stage("reparse pretty") {
      NOSJ.parse_native(pretty, nil)
    }.eql?(value)
    nil
  end

  # Rebrand an exception from a must-succeed stage with its context;
  # these are harness violations, not expected refusals.
  def stage(name)
    yield
  rescue => e
    raise "#{name} failed: #{e.class}: #{e.message}"
  end

  # --- lines: each_line vs one parse per line -------------------------

  def lines_case(bytes)
    s = utf8(bytes)
    yielded = []
    status = begin
      NOSJ.each_line_native(s, nil) { |v| yielded << v }
      :ok
    rescue *PARSE_FAIL
      :err
    end

    unless s.valid_encoding?
      raise "each_line accepted invalid UTF-8" unless status == :err
      raise "each_line yielded before the encoding gate" unless yielded.empty?
      return
    end

    reference = []
    ref_status = :ok
    s.split("\n", -1).each do |line|
      next if line.match?(/\A[ \t\r]*\z/)
      line_status, value = try_parse(line)
      if line_status == :err
        ref_status = :err
        break
      end
      reference << value
    end

    unless status == ref_status
      raise "acceptance: each_line #{status}, per-line parse #{ref_status}"
    end
    # On error the values yielded before the bad line must still match.
    raise "yielded values diverge from per-line parses" unless yielded.eql?(reference)

    if status == :ok && !yielded.empty?
      begin
        ndjson = NOSJ.generate_lines_native(yielded, nil)
      rescue NOSJ::GeneratorError
        return # WTF-8 strings (lone surrogates) are not generable
      end
      back = []
      NOSJ.each_line_native(ndjson, nil) { |v| back << v }
      raise "generate_lines does not round-trip" unless back.eql?(yielded)
    end
    nil
  end

  # --- patch/splice: raw-byte editing vs tree editing -----------------

  # Input is "document \0 spec"; a spec parsing to an object fuzzes
  # splice (pointer => value), an array fuzzes RFC 6902 patch.
  #
  # The reference tree parses under the default nesting cap, so
  # deeper-than-100 documents are exercised for crashes only, like
  # malformed ones: resolution scans just the bytes some pointer needs
  # (documented crate semantics), so the native side may legitimately
  # succeed where whole-document parsing refuses.
  def patch_case(bytes)
    doc_bytes, sep, spec_bytes = bytes.partition("\0")
    return if sep.empty?
    doc = utf8(doc_bytes)
    spec_status, spec = try_parse(utf8(spec_bytes))
    return unless spec_status == :ok
    # Replacement values with broken encoding (lone surrogates) raise
    # GeneratorError on insertion; the reference cannot mirror that.
    return unless deep_valid_encoding?(spec)

    tree_status, tree = try_parse(doc)
    # Duplicate keys: raw-byte resolution sees the first occurrence,
    # tree materialization keeps the last; the two sides cannot agree.
    if tree_status == :ok && NOSJ.stats_native(doc, nil)[:keys] != count_keys(tree)
      return
    end

    case spec
    when Hash
      splice_case(doc, tree_status, tree, spec)
    when Array
      return if spec.length > MAX_PATCH_OPS
      rfc6902_case(doc, tree_status, tree, spec)
    end
    nil
  end

  def splice_case(doc, tree_status, tree, edits)
    native_status, result = begin
      [:ok, NOSJ.splice_native(doc, edits, nil)]
    rescue *EDIT_FAIL
      [:err, nil]
    end
    return unless tree_status == :ok

    ref_status, ref = begin
      [:ok, ref_splice(tree, edits)]
    rescue RefFail
      [:err, nil]
    end
    unless native_status == ref_status
      raise "splice acceptance: native #{native_status}, reference #{ref_status}"
    end
    return unless native_status == :ok
    got = NOSJ.parse_native(result, {max_nesting: false})
    raise "splice result diverges from reference" unless got.eql?(ref)
  end

  def rfc6902_case(doc, tree_status, tree, ops)
    native_status, result = begin
      [:ok, NOSJ.patch_native(doc, ops, nil)]
    rescue *EDIT_FAIL
      [:err, nil]
    end
    return unless tree_status == :ok

    ref_status, ref = begin
      [:ok, ref_patch(tree, ops)]
    rescue RefFail
      [:err, nil]
    end
    unless native_status == ref_status
      raise "patch acceptance: native #{native_status}, reference #{ref_status}"
    end
    return unless native_status == :ok
    got = NOSJ.parse_native(result, {max_nesting: false})
    raise "patch result diverges from reference" unless got.eql?(ref)
  end

  # --- the pure-Ruby reference implementations ------------------------

  def ref_splice(tree, edits)
    token_lists = edits.keys.map { |ptr| ptr_tokens(ptr) }
    token_lists.combination(2) do |a, b|
      short, long = (a.length <= b.length) ? [a, b] : [b, a]
      raise RefFail if long[0, short.length] == short
    end
    box = [tree]
    edits.each_value.with_index do |value, i|
      toks = token_lists[i]
      if toks.empty?
        box[0] = value
      else
        parent = ref_resolve(box[0], toks[0..-2])
        case parent
        when Hash
          raise RefFail unless parent.key?(toks[-1])
          parent[toks[-1]] = value
        when Array
          parent[strict_index(toks[-1], parent.size)] = value
        else
          raise RefFail
        end
      end
    end
    box[0]
  end

  def ref_patch(tree, ops)
    box = [tree]
    ops.each do |op|
      raise RefFail unless op.is_a?(Hash)
      name = op["op"]
      raise RefFail unless name.is_a?(String)
      path = op["path"]
      raise RefFail unless path.is_a?(String)
      case name
      when "add" then ref_add(box, ptr_tokens(path), fetch_value(op))
      when "replace" then ref_replace(box, ptr_tokens(path), fetch_value(op))
      when "remove" then ref_remove(box, ptr_tokens(path))
      when "move"
        from = op["from"]
        raise RefFail unless from.is_a?(String)
        # The native side short-circuits these before validating the
        # pointers, in this order; mirror it exactly.
        next if path == from
        raise RefFail if path.start_with?("#{from}/") || from.empty?
        from_toks = ptr_tokens(from)
        moved = ref_resolve(box[0], from_toks)
        ref_remove(box, from_toks)
        ref_add(box, ptr_tokens(path), moved)
      when "copy"
        from = op["from"]
        raise RefFail unless from.is_a?(String)
        ref_add(box, ptr_tokens(path), deep_dup(ref_resolve(box[0], ptr_tokens(from))))
      when "test"
        raise RefFail unless ref_resolve(box[0], ptr_tokens(path)) == fetch_value(op)
      else
        raise RefFail
      end
    end
    box[0]
  end

  def ptr_tokens(ptr)
    return [] if ptr.empty?
    raise RefFail unless ptr.start_with?("/")
    # RFC 6901 unescape order: ~1 before ~0, matching the native side.
    ptr.split("/", -1).drop(1).map { |t| t.gsub("~1", "/").gsub("~0", "~") }
  end

  def strict_index(token, size, insert: false)
    raise RefFail unless token.match?(/\A(?:0|[1-9][0-9]*)\z/)
    i = token.to_i
    in_range = i < size || (insert && i == size)
    raise RefFail unless in_range
    i
  end

  def ref_resolve(node, toks)
    toks.each do |t|
      case node
      when Hash
        raise RefFail unless node.key?(t)
        node = node[t]
      when Array
        node = node[strict_index(t, node.size)]
      else
        raise RefFail
      end
    end
    node
  end

  def ref_add(box, toks, value)
    return box[0] = value if toks.empty?
    parent = ref_resolve(box[0], toks[0..-2])
    case parent
    when Hash
      parent[toks[-1]] = value
    when Array
      i = (toks[-1] == "-") ? parent.size : strict_index(toks[-1], parent.size, insert: true)
      parent.insert(i, value)
    else
      raise RefFail
    end
  end

  def ref_replace(box, toks, value)
    return box[0] = value if toks.empty?
    parent = ref_resolve(box[0], toks[0..-2])
    case parent
    when Hash
      raise RefFail unless parent.key?(toks[-1])
      parent[toks[-1]] = value
    when Array
      parent[strict_index(toks[-1], parent.size)] = value
    else
      raise RefFail
    end
  end

  def ref_remove(box, toks)
    raise RefFail if toks.empty?
    parent = ref_resolve(box[0], toks[0..-2])
    case parent
    when Hash
      raise RefFail unless parent.key?(toks[-1])
      parent.delete(toks[-1])
    when Array
      parent.delete_at(strict_index(toks[-1], parent.size))
    else
      raise RefFail
    end
  end

  def fetch_value(op)
    raise RefFail unless op.key?("value")
    op["value"]
  end

  def deep_dup(v)
    case v
    when Hash then v.transform_values { |e| deep_dup(e) }
    when Array then v.map { |e| deep_dup(e) }
    else v
    end
  end

  def deep_valid_encoding?(v)
    case v
    when String then v.valid_encoding?
    when Array then v.all? { |e| deep_valid_encoding?(e) }
    when Hash then v.all? { |k, e| deep_valid_encoding?(k) && deep_valid_encoding?(e) }
    else true
    end
  end

  def count_keys(v)
    case v
    when Hash then v.size + v.sum { |_, e| count_keys(e) }
    when Array then v.sum { |e| count_keys(e) }
    else 0
    end
  end
end
