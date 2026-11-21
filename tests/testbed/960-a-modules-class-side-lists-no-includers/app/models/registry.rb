# A plain module: `descendants` here is the module's own method, not
# Class#descendants, and an includer is no subclass.
module Registry
  def self.descendants
    []
  end

  def self.all
    descendants
  end
end
