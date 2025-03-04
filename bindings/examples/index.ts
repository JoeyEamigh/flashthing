import { FlashThing, type FlashEvent } from '../dist';

const archivePath = process.argv[2];
if (!archivePath) {
  console.error('Please provide an archive path as the first argument');
  process.exit(1);
}

const callback = (event: FlashEvent) => {
  console.log('Flash event:', event);
};

try {
  const flasher = new FlashThing(callback);
  await flasher.openArchive(archivePath);

  console.log(`Total flashing steps: ${flasher.getNumSteps()}`);
  await flasher.flash();

  console.log('Flashing completed successfully!');
} catch (error) {
  console.error('Error:', error);
}
