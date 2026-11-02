class Factory
  def self.build(kind)
    Object.const_get(kind).new
  end
end
