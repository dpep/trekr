class Tally
  def initialize(items)
    @sum = compute(items)
  end

  def compute(items)
    items.sum * 2
  end

  def total
    @sum
  end
end

class Mention
  PATTERN = /@\w+/

  def self.rewrite(text)
    text.gsub(PATTERN, "@x")
  end

  def self.scan(text)
    text.scan(PATTERN)
  end

  def self.slug(name)
    normalize(name)
  end

  def self.normalize(name)
    name.downcase
  end
end

Tally.new([1]).compute([1])
Mention.scan("a")
Mention.normalize("A")
