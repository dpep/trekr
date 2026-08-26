class Widget
  after_create :ensure_thing
  validate :check, if: :ready?

  def ensure_thing
  end

  def check
  end

  def ready?
  end

  def never_mentioned
  end
end
