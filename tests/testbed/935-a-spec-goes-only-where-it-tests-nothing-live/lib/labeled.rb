class Setup
  def self.registry!
    true
  end
end

class Labeled
  def label
    "l"
  end
end

Setup.registry!
