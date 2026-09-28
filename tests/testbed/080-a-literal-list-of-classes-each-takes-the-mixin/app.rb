module Generator
  def to_json
  end
end

Array.include(Generator)
Hash.include(Generator)

module Encoder
  def to_json
    super
  end
end

[Array, Hash].reverse_each do |klass|
  klass.include(Encoder)
end

{}.to_json

module Loud
  def to_s
  end
end

KINDS = [Symbol, Float].freeze

KINDS.each { |kind| kind.prepend(Loud) }

:name.to_s

module Quiet
  def to_s
  end
end

module Setup
  def self.install(kinds)
    [Integer].each { |kind| kind.prepend(Quiet) }
    kinds.each { |kind| kind.prepend(Quiet) }
  end
end

[String].each { |kind| kind.prepend(Quiet) if kind.frozen? }

1.to_s
"x".to_s
