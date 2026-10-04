module Passthrough
  def initialize(*)
    super
  end
end

class Entry < Vendor::Record
  def initialize(a)
    super
  end
end

class Lonely
  def initialize(a, b)
    @a = [a, b]
  end
end

class Built
  def initialize(a, b); end
end

Built.new(1, 2)
