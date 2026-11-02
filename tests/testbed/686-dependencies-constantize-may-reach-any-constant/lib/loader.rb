class Loader
  def self.load(name)
    ActiveSupport::Dependencies.constantize(name)
  end
end
