class Report
  def run(dir, name, text)
    Pathname.new(dir).join(name).read.upcase
    Digest::SHA256.hexdigest(text).upcase
    SecureRandom.hex.length
    Time.parse(text).year
    URI.parse(text).host
    Set.new.add(text).include?(name)
  end
end
