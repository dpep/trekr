class BadgeComponent
  def initialize(text)
    @text = text
  end

  private

  def badge_text
    @text.upcase
  end
end
