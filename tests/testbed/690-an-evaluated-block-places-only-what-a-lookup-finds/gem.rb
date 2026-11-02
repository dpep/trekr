module MyApp
  def self.setup
    GemThing.class_eval do
      include Extras
    end
  end

  def self.use
    GemThing.new.extra
  end
end
