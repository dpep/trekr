class Loader
  def self.load(name)
    Object.send(:const_get, name)
  end
end
